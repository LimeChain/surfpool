import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import crypto from "node:crypto";
import zlib from "node:zlib";
import { PublicKey } from "@solana/web3.js";

const RPC_URL =
  process.env.SURFPOOL_TEST_RPC_URL || "https://api.mainnet-beta.solana.com";
const MAX_ATTEMPTS = 5;
const RATE_LIMIT_CODES = new Set([-32005, -32429]);

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function isPubkey(str) {
  try {
    return new PublicKey(str).toBytes().length === 32;
  } catch {
    return false;
  }
}

async function rpc(method, params) {
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  let delay = 1000;
  for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
    let res;
    try {
      res = await fetch(RPC_URL, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
      });
    } catch (err) {
      if (attempt === MAX_ATTEMPTS) {
        throw new Error(`rpc: ${method} failed (${err.name || "NetworkError"})`);
      }
      await sleep(delay);
      delay *= 2;
      continue;
    }
    if (res.status === 429 || res.status >= 500) {
      if (attempt === MAX_ATTEMPTS) throw new Error(`rpc: ${method} failed (${res.status})`);
      await sleep(delay);
      delay *= 2;
      continue;
    }
    if (!res.ok) throw new Error(`rpc: ${method} failed (${res.status})`);
    const json = await res.json();
    if (json.error) {
      if (RATE_LIMIT_CODES.has(json.error.code) && attempt < MAX_ATTEMPTS) {
        await sleep(delay);
        delay *= 2;
        continue;
      }
      throw new Error(`rpc: ${method} failed (${json.error.code})`);
    }
    return json.result;
  }
}

async function getMultipleAccounts(addresses) {
  const results = new Array(addresses.length).fill(null);
  for (let i = 0; i < addresses.length; i += 100) {
    const chunk = addresses.slice(i, i + 100);
    const res = await rpc("getMultipleAccounts", [chunk, { encoding: "base64" }]);
    (res?.value || []).forEach((value, idx) => {
      if (!value) return;
      results[i + idx] = { owner: value.owner, data: Buffer.from(value.data[0], "base64"), lamports: value.lamports };
    });
  }
  return results;
}

async function lastSignatureTime(address) {
  const res = await rpc("getSignaturesForAddress", [address, { limit: 1 }]);
  return res && res.length > 0 ? res[0].blockTime ?? null : null;
}

const HERE = path.dirname(fileURLToPath(import.meta.url));
const TEMPLATES_ROOT = path.resolve(HERE, "../../../crates/core/src/scenarios/protocols");
const PROGRAMS_JSON_PATH = path.join(HERE, "programs.json");
const BPF_UPGRADEABLE_LOADER = "BPFLoaderUpgradeab1e11111111111111111111111";
const SYSTEM_PROGRAM = "11111111111111111111111111111111";
const STALE_SECONDS = 3 * 24 * 60 * 60;
const MAX_IDL_LEN = 16 * 1024 * 1024;

const stats = { total: 0, failed: 0 };
async function tracked(fn) {
  stats.total++;
  try {
    return await fn();
  } catch (err) {
    stats.failed++;
    throw err;
  }
}

function parseCheckArgs(argv) {
  const args = { snapshot: null, out: null };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--snapshot") args.snapshot = argv[++i];
    else if (argv[i] === "--out") args.out = argv[++i];
  }
  if (!args.snapshot || !args.out) throw new Error("usage: monitor.mjs check --snapshot <dir> --out <dir>");
  return args;
}
function walkTemplateDirs(root) {
  const found = [];
  (function walk(dir) {
    const entries = readdirSync(dir, { withFileTypes: true });
    if (entries.some((e) => e.isFile() && (e.name === "idl.json" || e.name === "overrides.yaml"))) found.push(dir);
    for (const e of entries) if (e.isDirectory()) walk(path.join(dir, e.name));
  })(root);
  return found;
}
function extractOverridesRefs(text, exclude) {
  const programIds = new Set();
  for (const m of text.matchAll(/^\s*program_id:\s*([1-9A-HJ-NP-Za-km-z]{32,44})/gm)) programIds.add(m[1]);
  const markets = new Set();
  for (const m of text.matchAll(/\b[1-9A-HJ-NP-Za-km-z]{32,44}\b/g)) {
    const token = m[0];
    if (programIds.has(token) || exclude.has(token) || token.startsWith("Sysvar") || token === SYSTEM_PROGRAM) continue;
    if (isPubkey(token)) markets.add(token);
  }
  return { programIds, markets };
}
function buildProtocols() {
  const protocols = new Map();
  for (const dir of walkTemplateDirs(TEMPLATES_ROOT)) {
    const key = path.relative(TEMPLATES_ROOT, dir).split(path.sep).join("/");
    const idlPath = path.join(dir, "idl.json");
    const overridesPath = path.join(dir, "overrides.yaml");
    const idl = existsSync(idlPath) ? JSON.parse(readFileSync(idlPath, "utf8")) : null;
    const overridesText = existsSync(overridesPath) ? readFileSync(overridesPath, "utf8") : "";
    const exclude = new Set(idl?.address ? [idl.address] : []);
    const { programIds, markets } = extractOverridesRefs(overridesText, exclude);
    if (idl?.address) programIds.add(idl.address);
    protocols.set(key, { idl, programIds, markets });
  }
  if (existsSync(PROGRAMS_JSON_PATH)) {
    for (const [key, entry] of Object.entries(JSON.parse(readFileSync(PROGRAMS_JSON_PATH, "utf8")))) {
      if (!protocols.has(key)) protocols.set(key, { idl: null, programIds: new Set(), markets: new Set() });
      const proto = protocols.get(key);
      for (const p of entry.programs || []) proto.programIds.add(p);
      for (const m of entry.markets || []) proto.markets.add(m);
    }
  }
  return protocols;
}

const trimTrailingZeros = (buf) => {
  let end = buf.length;
  while (end > 0 && buf[end - 1] === 0) end--;
  return buf.subarray(0, end);
};
const sha256Hex = (buf) => crypto.createHash("sha256").update(buf).digest("hex");
const elfObservation = (programId, loader, slot, authority, elfBuf) => {
  const elf = trimTrailingZeros(elfBuf);
  return { program_id: programId, loader, slot, authority, elf_sha256: sha256Hex(elf), elf_len: elf.length };
};
const fetchAccountsTracked = (addresses) => (addresses.length === 0 ? Promise.resolve([]) : tracked(() => getMultipleAccounts(addresses)));
async function buildProgramObservations(programIds) {
  const observations = new Map();
  if (programIds.length === 0) return observations;
  const accounts = await fetchAccountsTracked(programIds);
  const programDataNeeded = [];
  programIds.forEach((pid, i) => {
    const acc = accounts[i];
    if (!acc) observations.set(pid, null);
    else if (acc.owner === BPF_UPGRADEABLE_LOADER) programDataNeeded.push({ pid, pdAddress: new PublicKey(acc.data.subarray(4, 36)).toBase58() });
    else observations.set(pid, elfObservation(pid, "fixed", null, null, acc.data));
  });
  if (programDataNeeded.length > 0) {
    const pdAccounts = await fetchAccountsTracked(programDataNeeded.map((x) => x.pdAddress));
    programDataNeeded.forEach(({ pid }, i) => {
      const pdAcc = pdAccounts[i];
      if (!pdAcc) return observations.set(pid, null);
      const slot = Number(pdAcc.data.readBigUInt64LE(4));
      const authority = pdAcc.data[12] === 1 ? new PublicKey(pdAcc.data.subarray(13, 45)).toBase58() : null;
      observations.set(pid, elfObservation(pid, "upgradeable", slot, authority, pdAcc.data.subarray(45)));
    });
  }
  return observations;
}
function loadPreviousPrograms(snapshotDir, key) {
  const file = path.join(snapshotDir, key, "programs.json");
  if (!existsSync(file)) return {};
  try {
    return JSON.parse(readFileSync(file, "utf8"));
  } catch {
    return {};
  }
}
const programChanged = (a, b) => a.elf_sha256 !== b.elf_sha256 || a.slot !== b.slot || a.authority !== b.authority;
function writeJson(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, JSON.stringify(value, null, 2));
}
function programsFindingsForProtocol(key, programIds, observations, snapshotDir, outDir) {
  if (programIds.length === 0) return [];
  const previous = loadPreviousPrograms(snapshotDir, key);
  const current = {};
  const findings = programIds.map((pid) => {
    const obs = observations.get(pid) ?? null;
    current[pid] = obs;
    const prev = previous[pid];
    const status = !obs ? "missing" : !prev ? "first" : programChanged(obs, prev) ? "changed" : "unchanged";
    return { protocol: key, program_id: pid, status, current: obs, previous: prev ?? null };
  });
  writeJson(path.join(outDir, "snapshot.next", key, "programs.json"), current);
  return findings;
}

const normName = (name) => String(name).toLowerCase().replace(/_/g, "");
function normalizeType(type) {
  if (type === "publicKey" || type === "pubkey") return "pubkey";
  if (typeof type === "string") return type;
  if (type.defined !== undefined) return { defined: { name: typeof type.defined === "string" ? type.defined : type.defined.name } };
  if (type.option !== undefined) return { option: normalizeType(type.option) };
  if (type.vec !== undefined) return { vec: normalizeType(type.vec) };
  if (type.array !== undefined) return { array: [normalizeType(type.array[0]), type.array[1]] };
  if (type.coption !== undefined) return { coption: normalizeType(type.coption) };
  if (type.generic !== undefined) return { generic: type.generic };
  return type;
}
function stableStringify(value) {
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((k) => `${JSON.stringify(k)}:${stableStringify(value[k])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}
const typeKey = (type) => stableStringify(normalizeType(type));
// Tuple structs list bare types instead of {name, type}; index them positionally.
const fieldEntry = (field, index) => (field !== null && typeof field === "object" && "name" in field ? [field.name, typeKey(field.type)] : [String(index), typeKey(field)]);
const isLegacyIdl = (idl) => !idl.metadata || !idl.metadata.spec;
function findMatchingType(typesByName, name) {
  if (typesByName.has(name)) return typesByName.get(name);
  const norm = normName(name);
  for (const [k, v] of typesByName) if (normName(k) === norm) return v;
  return null;
}
function canonicalizeIdl(idl) {
  const legacy = isLegacyIdl(idl);
  const typesByName = new Map((idl.types || []).map((t) => [t.name, t]));
  const types = {};
  for (const t of idl.types || []) {
    types[t.name] = t.type?.kind === "enum"
      ? { kind: "enum", variants: (t.type.variants || []).map((v) => v.name) }
      : { kind: "struct", fields: (t.type?.fields || []).map(fieldEntry) };
  }
  // legacy IDLs embed an account's struct inline instead of in types[]; fold it in under the account name.
  const accounts = {};
  for (const acc of idl.accounts || []) {
    const existingType = findMatchingType(typesByName, acc.name);
    const fieldsSource = existingType?.type?.fields || acc.type?.fields || [];
    accounts[acc.name] = { discriminator: legacy ? null : acc.discriminator ?? null, fields: fieldsSource.map(fieldEntry) };
    if (!existingType) types[acc.name] = { kind: "struct", fields: fieldsSource.map(fieldEntry) };
  }
  const instructions = {};
  for (const ix of idl.instructions || []) {
    instructions[ix.name] = {
      discriminator: legacy ? null : ix.discriminator ?? null,
      args: (ix.args || []).map((a) => [a.name, typeKey(a.type)]),
      accounts: (ix.accounts || []).map((a) => a.name),
    };
  }
  return { version: legacy ? idl.version ?? null : idl.metadata?.version ?? null, accounts, types, instructions };
}

function byNormName(map) {
  const out = new Map();
  for (const [name, def] of Object.entries(map)) out.set(normName(name), { name, def });
  return out;
}
function compareFields(entityName, committedFields, publishedFields, errors, infos) {
  const pubMap = new Map(publishedFields.map(([name, tkey]) => [normName(name), { name, tkey }]));
  const comMap = new Map(committedFields.map(([name, tkey]) => [normName(name), { name, tkey }]));
  for (const [key, { name, tkey }] of comMap) {
    const pub = pubMap.get(key);
    if (!pub) errors.push(`${entityName}.${name} removed`);
    else if (pub.tkey !== tkey) errors.push(`${entityName}.${name} changed: ${tkey} -> ${pub.tkey}`);
  }
  for (const [key, { name }] of pubMap) if (!comMap.has(key)) infos.push(`new ${entityName}.${name}`);
}
function diffEntities(kind, committed, published, compareOne, errors, infos) {
  const comMap = byNormName(committed);
  const pubMap = byNormName(published);
  for (const [key, { name, def }] of comMap) {
    const pub = pubMap.get(key);
    if (!pub) errors.push(`${kind} ${name} is no longer published`);
    else compareOne(name, def, pub.def, errors, infos);
  }
  for (const [key, { name }] of pubMap) if (!comMap.has(key)) infos.push(`new ${kind} ${name}`);
}
function compareDiscriminator(kind, name, def, pubDef, hasDisc, errors) {
  if (hasDisc && def.discriminator && pubDef.discriminator && JSON.stringify(def.discriminator) !== JSON.stringify(pubDef.discriminator)) {
    errors.push(`${kind} ${name} discriminator changed`);
  }
}
const listsDiffer = (a, b) => JSON.stringify(a) !== JSON.stringify(b);
function compareIdls(committed, published, publishedIsLegacy) {
  const errors = [];
  const infos = [];
  const hasDisc = !publishedIsLegacy;
  if (committed.version !== published.version) infos.push(`version differs: ${committed.version} -> ${published.version}`);
  diffEntities("account", committed.accounts, published.accounts, (name, def, pub, errs, infs) => {
    compareDiscriminator("account", name, def, pub, hasDisc, errs);
    compareFields(name, def.fields, pub.fields, errs, infs);
  }, errors, infos);
  const withoutAccounts = (idl) => {
    const accountNames = new Set(Object.keys(idl.accounts).map(normName));
    return Object.fromEntries(Object.entries(idl.types).filter(([name]) => !accountNames.has(normName(name))));
  };
  diffEntities("type", withoutAccounts(committed), withoutAccounts(published), (name, def, pub, errs, infs) => {
    if (def.kind === "enum" || pub.kind === "enum") {
      const comV = new Set((def.variants || []).map(normName));
      const pubV = new Set((pub.variants || []).map(normName));
      for (const v of def.variants || []) if (!pubV.has(normName(v))) errs.push(`${name}.${v} variant removed`);
      for (const v of pub.variants || []) if (!comV.has(normName(v))) infs.push(`new ${name}.${v} variant`);
    } else {
      compareFields(name, def.fields || [], pub.fields || [], errs, infs);
    }
  }, errors, infos);
  diffEntities("instruction", committed.instructions, published.instructions, (name, def, pub, errs) => {
    compareDiscriminator("instruction", name, def, pub, hasDisc, errs);
    if (listsDiffer(def.args.map(([n]) => normName(n)), pub.args.map(([n]) => normName(n))) || listsDiffer(def.args.map(([, t]) => t), pub.args.map(([, t]) => t))) {
      errs.push(`instruction ${name} args changed`);
    }
    if (listsDiffer(def.accounts.map(normName), pub.accounts.map(normName))) errs.push(`instruction ${name} accounts changed`);
  }, errors, infos);
  return { errors, infos };
}

function idlFinding(protocol, programId, status, opts = {}) {
  return { protocol, program_id: programId, status, published_schema: opts.schema ?? null, errors: opts.errors ?? [], infos: opts.infos ?? [], published_file: opts.file ?? null };
}
const fetchIdlAccount = (address) => tracked(async () => (await getMultipleAccounts([address]))[0]);
async function idlFindingForProtocol(key, idl, outDir) {
  const programId = idl.address;
  if (!programId) return idlFinding(key, null, "no_address");
  const pk = new PublicKey(programId);
  const base = PublicKey.findProgramAddressSync([], pk)[0];
  const idlAddress = await PublicKey.createWithSeed(base, "anchor:idl", pk);
  const acc = await fetchIdlAccount(idlAddress.toBase58()).catch(() => undefined);
  if (acc === undefined) return idlFinding(key, programId, "unknown");
  if (!acc) return idlFinding(key, programId, "unpublished");
  const len = acc.data.readUInt32LE(40);
  if (len > MAX_IDL_LEN) return idlFinding(key, programId, "unknown", { errors: [`idl too large: ${len} bytes`] });
  const publishedIdl = JSON.parse(zlib.inflateSync(acc.data.subarray(44, 44 + len)).toString("utf8"));
  const publishedIsLegacy = isLegacyIdl(publishedIdl);
  const { errors, infos } = compareIdls(canonicalizeIdl(idl), canonicalizeIdl(publishedIdl), publishedIsLegacy);
  const drift = errors.length > 0 || infos.length > 0;
  let file = null;
  if (drift) {
    file = `idls/${key.split("/").join("-")}.json`;
    writeJson(path.join(outDir, file), publishedIdl);
  }
  return idlFinding(key, programId, drift ? "drift" : "match", { schema: publishedIsLegacy ? "legacy" : "0.30", errors, infos, file });
}

async function marketFinding(protocol, address, accountsByAddress) {
  const acc = accountsByAddress.get(address);
  if (acc === undefined) return { protocol, address, status: "unknown", last_activity: null };
  if (acc === null) return { protocol, address, status: "missing", last_activity: null };
  const ts = await tracked(() => lastSignatureTime(address)).catch(() => undefined);
  if (ts === undefined) return { protocol, address, status: "unknown", last_activity: null };
  if (ts === null) return { protocol, address, status: "stale", last_activity: null };
  const isStale = Date.now() / 1000 - ts > STALE_SECONDS;
  return { protocol, address, status: isStale ? "stale" : "active", last_activity: new Date(ts * 1000).toISOString() };
}
async function runWithConcurrency(items, limit, pauseMs, worker) {
  const results = [];
  for (let i = 0; i < items.length; i += limit) {
    results.push(...(await Promise.all(items.slice(i, i + limit).map(worker))));
    if (i + limit < items.length) await sleep(pauseMs);
  }
  return results;
}

const emptyFindings = (rpcOk) => ({ generated_at: new Date().toISOString(), rpc_ok: rpcOk, protocols: [], programs: [], idls: [], markets: [], not_covered: [] });

async function runCheck(argv) {
  const args = parseCheckArgs(argv);
  const protocols = buildProtocols();
  const protocolKeys = [...protocols.keys()].sort();
  const allProgramIds = [...new Set([...protocols.values()].flatMap((p) => [...p.programIds]))];
  let observations;
  try {
    observations = await buildProgramObservations(allProgramIds);
  } catch {
    writeJson(path.join(args.out, "findings.json"), emptyFindings(false));
    console.log("rpc unreachable on first request, wrote empty findings");
    return;
  }
  const programFindings = [];
  const idlFindings = [];
  const marketFindings = [];
  const notCovered = [];
  for (const key of protocolKeys) {
    const proto = protocols.get(key);
    const programIds = [...proto.programIds];
    if (programIds.length === 0) notCovered.push(`${key}: no program id in idl.json or templates; add one to .github/scripts/monitoring/programs.json`);
    programFindings.push(...programsFindingsForProtocol(key, programIds, observations, args.snapshot, args.out));
    let idlLine = "no idl";
    if (proto.idl) {
      const finding = await idlFindingForProtocol(key, proto.idl, args.out).catch(() => idlFinding(key, proto.idl.address ?? null, "unknown"));
      idlFindings.push(finding);
      idlLine = `idl ${finding.status}`;
    }
    const marketAddresses = [...proto.markets];
    const marketAccounts = marketAddresses.length > 0 ? await fetchAccountsTracked(marketAddresses).catch(() => null) : [];
    const accountsByAddress = new Map(marketAddresses.map((addr, i) => [addr, marketAccounts ? marketAccounts[i] ?? null : undefined]));
    marketFindings.push(...(await runWithConcurrency(marketAddresses, 1, 300, (addr) => marketFinding(key, addr, accountsByAddress))));
    console.log(`${key}: ${programIds.length} program${programIds.length === 1 ? "" : "s"}, ${idlLine}, ${marketAddresses.length} markets`);
  }
  const rpcOk = stats.total === 0 || stats.failed <= stats.total / 2;
  writeJson(path.join(args.out, "findings.json"), { generated_at: new Date().toISOString(), rpc_ok: rpcOk, protocols: protocolKeys, programs: programFindings, idls: idlFindings, markets: marketFindings, not_covered: notCovered });
  console.log(`done: ${protocolKeys.length} protocols, ${programFindings.length} programs, ${idlFindings.length} idls, ${marketFindings.length} markets, rpc_ok=${rpcOk}`);
}

function parseRenderArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 2) args[argv[i].replace(/^--/, '')] = argv[i + 1];
  return args;
}

function readSafe(path, asJson) {
  try {
    const text = readFileSync(path, 'utf8');
    return asJson ? JSON.parse(text) : text;
  } catch {
    return null;
  }
}

function esc(cell) {
  return String(cell ?? '').replace(/\|/g, '\\|');
}

function short(value) {
  const s = String(value ?? '');
  return s.length <= 14 ? s : `${s.slice(0, 8)}…${s.slice(-4)}`;
}

const ENV_FAILURE_RES = [
  /kind:\s*Reqwest\([\s\S]*?127\.0\.0\.1[\s\S]*?TimedOut/,
  /error sending request for url/,
];
const isEnvFailure = (detail) => ENV_FAILURE_RES.some((re) => re.test(detail));

function parseTestsLog(text) {
  if (text === null) return { ran: false, cutShort: false, suites: new Map(), failures: new Map() };
  const resultRe = /^test (\S+) \.\.\. (ok|FAILED|ignored)(?:,.*)?$/gm;
  const results = [...text.matchAll(resultRe)].map((m) => ({ name: m[1], outcome: m[2] }));
  const ran = /^test result:/m.test(text) || results.length > 0;
  const cutShort = !/^test result:/m.test(text) && results.length > 0;
  const failureDetails = new Map();
  const failuresIdx = text.indexOf('\nfailures:\n');
  if (failuresIdx !== -1) {
    const blockRe = /---- (\S+) stdout ----\n([\s\S]*?)(?=\n----|\nfailures:|$)/g;
    for (const m of text.slice(failuresIdx).matchAll(blockRe)) failureDetails.set(m[1], m[2].trim());
  }
  const suites = new Map();
  const failures = new Map();
  for (const { name, outcome } of results) {
    const suite = name.startsWith('tests::') ? name.split('::')[1] : 'unit';
    if (!suites.has(suite)) suites.set(suite, { passed: 0, failed: 0, unverified: 0, ignored: 0 });
    const s = suites.get(suite);
    if (outcome === 'ok') s.passed += 1;
    else if (outcome === 'ignored') s.ignored += 1;
    else {
      const detail = failureDetails.get(name) ?? '';
      const env = isEnvFailure(detail);
      env ? (s.unverified += 1) : (s.failed += 1);
      failures.set(name, { detail, env });
    }
  }
  return { ran, cutShort, suites, failures };
}

function computeStatus(findings, tests) {
  if (!findings || findings.rpc_ok === false) return 'incomplete';
  const programDrift = (findings.programs ?? []).some((p) => p.status === 'changed' || p.status === 'missing');
  const idlDrift = (findings.idls ?? []).some((i) => (i.errors ?? []).length > 0);
  const marketDrift = (findings.markets ?? []).some((m) => m.status === 'missing');
  const testDrift = [...tests.failures.values()].some((f) => !f.env);
  if (programDrift || idlDrift || marketDrift || testDrift) return 'drift';
  const envFailure = [...tests.failures.values()].some((f) => f.env);
  const anyUnknown =
    (findings.programs ?? []).some((p) => p.status === 'unknown') ||
    (findings.idls ?? []).some((i) => i.status === 'unknown') ||
    (findings.markets ?? []).some((m) => m.status === 'unknown');
  if (envFailure || anyUnknown || !tests.ran || tests.cutShort) return 'unverified';
  return 'clean';
}

function renderPrograms(programs) {
  if (!programs?.length) return 'checks did not run\n';
  const changed = programs.filter((p) => p.status === 'changed');
  const first = programs.filter((p) => p.status === 'first');
  const rest = programs.filter((p) => p.status !== 'changed' && p.status !== 'first');
  const rows = ['| Protocol | Program | Loader | Slot | Authority | ELF sha256 | Status |', '| --- | --- | --- | --- | --- | --- | --- |'];
  for (const p of [...changed, ...rest, ...first]) {
    const cur = p.current ?? {};
    rows.push(
      `| ${esc(p.protocol)} | ${esc(short(p.program_id))} | ${esc(cur.loader)} | ${esc(cur.slot)} | ` +
        `${esc(short(cur.authority))} | ${esc(short(cur.elf_sha256))} | ${esc(p.status)} |`,
    );
  }
  const details = [];
  for (const p of changed) {
    details.push(`- ${esc(p.protocol)} ${esc(short(p.program_id))}`);
    for (const field of ['loader', 'slot', 'authority', 'elf_sha256', 'elf_len']) {
      if (p.previous?.[field] !== p.current?.[field]) {
        details.push(`  - ${field}: ${esc(p.previous?.[field])} → ${esc(p.current?.[field])}`);
      }
    }
  }
  if (details.length) rows.push('', 'Changed since the previous run', ...details);
  if (first.length) rows.push('', `First observation for ${first.length} program(s); recorded for the next run.`);
  return rows.join('\n') + '\n';
}

function renderIdls(idls) {
  if (!idls?.length) return 'checks did not run\n';
  const lines = [];
  const notCompared = [];
  for (const idl of idls) {
    if (idl.status === 'unpublished' || idl.status === 'no_address') {
      notCompared.push(idl);
      continue;
    }
    const schema = idl.published_schema ? `, published ${idl.published_schema}` : '';
    lines.push(`- **${esc(idl.protocol)}** (${esc(short(idl.program_id))}) — ${esc(idl.status)}${schema}`);
    for (const e of idl.errors ?? []) lines.push(`  - error: ${esc(e)}`);
    const infos = idl.infos ?? [];
    if (infos.length) lines.push(`  - info: ${infos.length} additions in the published IDL (new fields, types or instructions), not in the committed copy`);
    if (idl.published_file) lines.push(`  - published copy: ${esc(idl.published_file)}`);
  }
  if (notCompared.length > 0) {
    lines.push('- Not compared');
    for (const idl of notCompared) lines.push(`  - ${esc(idl.protocol)} (${esc(short(idl.program_id))}) — ${esc(idl.status)}`);
  }
  return lines.length > 0 ? lines.join('\n') + '\n' : 'none\n';
}

function renderMarkets(markets) {
  if (!markets?.length) return 'checks did not run\n';
  const byStatus = (status) => markets.filter((m) => m.status === status);
  const active = byStatus('active');
  const missing = byStatus('missing');
  const stale = byStatus('stale');
  const unknown = byStatus('unknown');
  const lines = [`Active: ${active.length}`, '', 'Missing'];
  lines.push(...(missing.length ? missing.map((m) => `- ${esc(m.protocol)}, ${esc(m.address)}`) : ['- none']));
  lines.push('', 'Stale (no transaction in 3 days)');
  lines.push(
    ...(stale.length
      ? stale.map((m) => `- ${esc(m.protocol)}, ${esc(m.address)} — last activity ${esc(m.last_activity)}`)
      : ['- none']),
  );
  if (unknown.length > 0) {
    lines.push('', 'Could not check');
    lines.push(...unknown.map((m) => `- ${esc(m.protocol)}, ${esc(m.address)}`));
  }
  return lines.join('\n') + '\n';
}

function panicMessage(detail) {
  const lines = detail.split('\n').map((l) => l.trim()).filter(Boolean);
  const at = lines.findIndex((l) => /panicked at/.test(l));
  const message = lines[at + 1] ?? lines[0] ?? '';
  return message ? `: ${esc(message)}` : '';
}

function renderTests(tests) {
  if (!tests.ran) return 'Suites did not run\n';
  const rows = ['| Suite | Passed | Failed | Unverified | Ignored |', '| --- | --- | --- | --- | --- |'];
  for (const name of [...tests.suites.keys()].sort()) {
    const s = tests.suites.get(name);
    rows.push(`| ${esc(name)} | ${s.passed} | ${s.failed} | ${s.unverified} | ${s.ignored} |`);
  }
  const cutShortNote = tests.cutShort ? '\nrun was cut short\n' : '';
  const failureLines = [];
  for (const [name, f] of tests.failures) {
    if (f.env) {
      failureLines.push(`- ${esc(name)} — unverified (environment)`);
    } else {
      failureLines.push(`- ${esc(name)} — failed${panicMessage(f.detail)}`);
    }
  }
  const failuresBlock = failureLines.length > 0 ? '\n' + failureLines.join('\n') + '\n' : '';
  return rows.join('\n') + '\n' + cutShortNote + failuresBlock;
}

function renderRunDetails(args, findings) {
  const notCovered = findings?.not_covered ?? [];
  const lines = [
    `- Timestamp: ${args.timestamp}`,
    `- Commit: ${args.commit}`,
    `- Run URL: ${args['run-url']}`,
    `- RPC reachable: ${findings ? (findings.rpc_ok ? 'yes' : 'no') : 'unknown'}`,
    `- Protocols checked: ${findings?.protocols?.length ?? 0}`,
  ];
  if (notCovered.length > 0) {
    lines.push('- Not covered:', ...notCovered.map((item) => `  - ${esc(item)}`));
  } else {
    lines.push('- Not covered: none');
  }
  return lines.join('\n') + '\n';
}

const VERDICT_SENTENCE = {
  incomplete: 'checks did not complete; treat this run as inconclusive.',
  drift: 'one or more monitored protocols no longer match what we ship.',
  unverified: 'nothing confirmed drifted, but part of the run could not be verified.',
  clean: 'all monitored protocols match what we ship.',
};

function runRender(argv) {
  const args = parseRenderArgs(argv);
  const findings = readSafe(args.findings, true);
  const tests = parseTestsLog(readSafe(args.tests, false));
  const status = computeStatus(findings, tests);
  const shortSha = String(args.commit ?? '').slice(0, 7);

  const report = [
    `# Protocol monitoring — ${args.timestamp}\n`,
    `**Verdict: ${status}** — ${VERDICT_SENTENCE[status]} Run: ${args['run-url']}. Commit: ${shortSha}.\n`,
    '## Program upgrades',
    findings ? renderPrograms(findings.programs) : 'checks did not run\n',
    '## IDLs',
    findings ? renderIdls(findings.idls) : 'checks did not run\n',
    '## Markets',
    findings ? renderMarkets(findings.markets) : 'checks did not run\n',
    '## Integration tests',
    renderTests(tests),
    '## Run details',
    renderRunDetails(args, findings),
  ].join('\n');

  mkdirSync(path.dirname(args.out), { recursive: true });
  writeFileSync(args.out, report);
  mkdirSync(path.dirname(args['status-out']), { recursive: true });
  writeFileSync(args['status-out'], status);
}

const [command, ...rest] = process.argv.slice(2);
const commands = { check: runCheck, render: runRender };
if (!commands[command]) {
  console.error("usage: monitor.mjs check|render ...");
  process.exit(2);
}
Promise.resolve(commands[command](rest)).catch((err) => {
  console.error(err.message || String(err));
  process.exit(1);
});
