#!/usr/bin/env bash
# Compares the protocols we integrate with what is deployed on mainnet.
# Usage: monitor.sh <previous snapshot dir> <output dir>
set -euo pipefail

snapshot_dir=$1
out_dir=$2
protocols_root=crates/core/src/scenarios/protocols
rpc_url=${SURFPOOL_TEST_RPC_URL:-https://api.mainnet-beta.solana.com}

# These protocols have no IDL in the repository and their templates carry no program id.
extra_programs=(
  bisonfi:BiSoNHVpsVZW2F7rx2eQ59yQwKxzU5NvBcmKshCSUypi
  humidifi:9H6tua7jkLhdm3w8BvgpTn5LZNU7g4ZynDmCiNN3q6Rp
  tessera:TessVdML9pBGgG9yGks7o4HewRaXVAMuoVj4x83GLQH
  goonfi:goonuddtQRrWqqn5nFyczVKaie28f3kDkHWkHtURSLE
  solfi:SV2EYYJyRz2YhfXwXnhNAevDEui5Q6yrfyo13WtupPF
)

# Committed IDLs are trimmed (most keep no instructions or errors), so only what a template can
# depend on is compared. Older IDLs spell account flags isMut/isSigner; the published ones are
# converted to writable/signer, so both sides are normalised the same way before comparing.
idl_pick='del(.. | .docs?)
  | walk(if type == "object"
      then with_entries(.key |= ({isMut: "writable", isSigner: "signer"}[.] // .))
      else . end)
  | {accounts: (.accounts // [] | sort_by(.name)),
     types: (.types // [] | sort_by(.name)),
     instructions: (.instructions // [] | sort_by(.name))}'
# Entries of one side that the other side does not publish identically, as "section/name".
idl_only_in_first='to_entries[] | .key as $section | .value[]
  | select(. as $entry | $other[0][$section] | index($entry) | not) | "\($section)/\(.name)"'

mkdir -p "$out_dir/snapshot/programs" "$out_dir/idls"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
programs=$work/programs.md
changes=$work/changes.md
idls=$work/idls.md
markets=$work/markets.txt
program_ids=$work/program_ids.txt
touch "$programs" "$changes" "$idls" "$markets" "$program_ids"
drift=false
gaps=false

check_program() {
  local name=$1 id=$2
  local new=$out_dir/snapshot/programs/${name//\//-}.json
  local old=$snapshot_dir/programs/${name//\//-}.json
  echo "$id" >> "$program_ids"
  if ! solana program show "$id" --url "$rpc_url" --output json 2>/dev/null \
      | jq '{lastDeploySlot, authority}' > "$new"; then
    rm -f "$new"
    echo "| $name | $id | | | could not read |" >> "$programs"
    gaps=true
    echo "$name: could not read"
    return
  fi
  local slot authority status
  slot=$(jq -r .lastDeploySlot "$new")
  authority=$(jq -r .authority "$new")
  if [ ! -f "$old" ]; then
    status="first observation"
  elif diff -q "$old" "$new" > /dev/null; then
    status=unchanged
  else
    status=changed
    drift=true
    echo "- $name: slot $(jq -r .lastDeploySlot "$old") -> $slot," \
      "authority $(jq -r .authority "$old") -> $authority" >> "$changes"
  fi
  echo "| $name | $id | $slot | $authority | $status |" >> "$programs"
  echo "$name: $status"
}

check_idl() {
  local name=$1 id=$2 committed=$3
  local safe=${name//\//-}
  local fetched=$work/$safe.json
  if [ -z "$id" ]; then
    echo "- $name: no program address in idl.json" >> "$idls"
    return
  fi
  if ! anchor idl fetch "$id" --provider.cluster "$rpc_url" -o "$fetched" > /dev/null 2>&1; then
    echo "- $name: no published IDL" >> "$idls"
    return
  fi
  if [ -z "$(jq -r '.metadata.spec // empty' "$fetched")" ]; then
    jq --arg id "$id" '.metadata.address = $id' "$fetched" > "$fetched.legacy"
    if ! anchor idl convert "$fetched.legacy" -o "$fetched" > /dev/null 2>&1; then
      echo "- $name: published IDL is legacy and could not be converted" >> "$idls"
      gaps=true
      return
    fi
  fi
  local sections='["accounts", "types", "instructions"]'
  if [ "$(jq '.instructions // [] | length' "$committed")" = 0 ]; then
    sections='["accounts", "types"]'
  fi
  local keep="with_entries(select(.key as \$key | $sections | index(\$key)))"
  local ours=$work/$safe.ours theirs=$work/$safe.theirs
  if ! jq -S "$idl_pick | $keep" "$committed" > "$ours" \
      || ! jq -S "$idl_pick | $keep" "$fetched" > "$theirs"; then
    echo "- $name: IDL is not valid JSON" >> "$idls"
    gaps=true
    return
  fi
  local diff_file=$out_dir/idls/$safe.diff
  if diff -d "$ours" "$theirs" > "$diff_file"; then
    rm "$diff_file"
    echo "- $name: match" >> "$idls"
    echo "$name: idl match"
    return
  fi
  cp "$fetched" "$out_dir/idls/$safe.json"
  local changed added
  changed=$(jq -r --slurpfile other "$theirs" "$idl_only_in_first" "$ours" | paste -sd ' ' -)
  added=$(jq -r --slurpfile other "$ours" "$idl_only_in_first" "$theirs" | grep -c '' || true)
  if [ -z "$changed" ]; then
    echo "- $name: $added entries added upstream, everything we ship is still published" \
      "([diff](idls/$safe.diff))" >> "$idls"
  else
    drift=true
    echo "- $name: **we ship entries the program no longer publishes as we have them:**" \
      "$changed; $added added upstream ([diff](idls/$safe.diff))" >> "$idls"
  fi
  echo "$name: idl differs"
}

check_market() {
  local name=$1 addr=$2
  local body response block_time
  body="{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getSignaturesForAddress\","
  body="$body\"params\":[\"$addr\",{\"limit\":1}]}"
  if ! response=$(curl -s --retry 3 --retry-delay 2 -H 'content-type: application/json' \
      -d "$body" "$rpc_url") || ! echo "$response" | jq -e 'has("result")' > /dev/null 2>&1; then
    echo "unchecked $name $addr" >> "$markets"
    gaps=true
    return
  fi
  block_time=$(echo "$response" | jq -r '.result[0].blockTime // empty')
  if [ -z "$block_time" ]; then
    echo "missing $name $addr" >> "$markets"
    drift=true
  elif [ $(( $(date +%s) - block_time )) -gt $(( 3 * 24 * 3600 )) ]; then
    echo "stale $name $addr" >> "$markets"
  else
    echo "active $name $addr" >> "$markets"
  fi
}

while IFS= read -r -d '' idl_file; do
  name=${idl_file#"$protocols_root"/}
  name=${name%/idl.json}
  id=$(jq -r '.address // empty' "$idl_file")
  if [ -n "$id" ]; then
    check_program "$name" "$id"
  fi
  check_idl "$name" "$id" "$idl_file"
done < <(find "$protocols_root" -name idl.json -print0 | sort -z)

for entry in "${extra_programs[@]}"; do
  check_program "${entry%%:*}" "${entry#*:}"
done

# Every address a template offers as a choice (option values and constants).
while IFS= read -r -d '' file; do
  name=${file#"$protocols_root"/}
  name=${name%/overrides.yaml}
  for addr in $(grep -hE '^ *(value|constant): *"?[1-9A-HJ-NP-Za-km-z]{43,44}"? *$' "$file" \
      | grep -oE '[1-9A-HJ-NP-Za-km-z]{43,44}' | sort -u || true); do
    if grep -qx "$addr" "$program_ids"; then
      continue
    fi
    check_market "$name" "$addr"
    sleep 0.3
  done
done < <(find "$protocols_root" -name overrides.yaml -print0 | sort -z)
echo "markets: $(grep -c '' "$markets") checked"

verdict=clean
if [ "$drift" = true ]; then
  verdict=drift
elif [ "$gaps" = true ]; then
  verdict=incomplete
fi

count() { grep -c "^$1 " "$markets" || true; }
list() { grep "^$1 " "$markets" | sed 's/^[a-z]* /- /' || echo "- none"; }
tests_summary() {
  if [ ! -f tests.log ]; then
    echo "not run"
    return
  fi
  grep -E '^ *(Summary|FAIL|TIMEOUT) ' tests.log | sort -u || echo "no summary in tests.log"
}
rpc_label=public
if [ -n "${SURFPOOL_TEST_RPC_URL:-}" ]; then
  rpc_label=custom
fi

{
  echo "# Protocol monitoring — $(date -u +'%Y-%m-%d %H:%M UTC')"
  echo
  echo "Verdict: **$verdict**"
  echo
  echo "## Program upgrades"
  echo "| Protocol | Program | Slot | Authority | Status |"
  echo "|---|---|---|---|---|"
  cat "$programs"
  echo
  cat "$changes"
  echo
  echo "## IDLs"
  cat "$idls"
  echo
  echo "## Markets"
  echo "Active: $(count active). Missing: $(count missing). Stale: $(count stale)." \
    "Could not check: $(count unchecked)."
  echo
  echo "Missing (no transaction ever):"
  list missing
  echo
  echo "Stale (no transaction in 3 days):"
  list stale
  echo
  echo "Could not check (RPC error):"
  list unchecked
  echo
  echo "## Integration tests"
  echo '```'
  tests_summary
  echo '```'
  echo
  echo "## Run details"
  echo "- RPC: $rpc_label"
  echo "- Programs checked: $(grep -c '' "$program_ids")"
} > "$out_dir/report.md"
echo "$verdict" > "$out_dir/status"
echo "verdict: $verdict"
