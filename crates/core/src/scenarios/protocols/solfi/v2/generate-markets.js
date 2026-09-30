#!/usr/bin/env node

/**
 * Regenerates the committed SolFi market, oracle and vault selectors.
 *
 * Usage:
 *   SOLANA_RPC_URL=https://api.mainnet-beta.solana.com node generate-markets.js
 *
 * Discovery happens only while maintaining the catalog. Surfpool and Studio never query the
 * network while serving scenario templates.
 */

const fs = require('fs');
const path = require('path');

const RPC_URL = process.env.SOLANA_RPC_URL || 'https://api.mainnet-beta.solana.com';
const SOLFI_PROGRAM = 'SV2EYYJyRz2YhfXwXnhNAevDEui5Q6yrfyo13WtupPF';
const MARKET_SIZE = 1728;
const DEFAULT_MARKETS = [
  '65ZHSArs5XxPseKQbB1B4r16vDxMWnCxHMzogDAqiDUc',
  'FkEB6uvyzuoaGpgs4yRtFtxC4WJxhejNFbUkj5R6wR32',
];
const FILES = {
  market: path.join(__dirname, 'market-overrides.yaml'),
  oracle: path.join(__dirname, 'oracle-overrides.yaml'),
  vault: path.join(__dirname, 'vault-overrides.yaml'),
};
const TOKENS_FILE = path.join(__dirname, '../../../../../../types/src/verified_tokens.csv');
const BASE58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const ZERO_PUBKEY = '11111111111111111111111111111111';

async function rpc(method, params) {
  const response = await fetch(RPC_URL, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  if (!response.ok) throw new Error(`${method} failed with HTTP ${response.status}`);
  const body = await response.json();
  if (body.error) throw new Error(`${method} failed: ${JSON.stringify(body.error)}`);
  return body.result;
}

function encodeBase58(bytes) {
  if (bytes.length === 0) return '';
  const digits = [0];
  for (const byte of bytes) {
    let carry = byte;
    for (let i = 0; i < digits.length; i++) {
      carry += digits[i] << 8;
      digits[i] = carry % 58;
      carry = Math.floor(carry / 58);
    }
    while (carry > 0) {
      digits.push(carry % 58);
      carry = Math.floor(carry / 58);
    }
  }
  let leadingZeroes = 0;
  while (leadingZeroes < bytes.length - 1 && bytes[leadingZeroes] === 0) leadingZeroes++;
  return '1'.repeat(leadingZeroes) + digits.reverse().map(digit => BASE58_ALPHABET[digit]).join('');
}

function pubkey(data, start) {
  return encodeBase58(data.subarray(start, start + 32));
}

function parseCsvLine(line) {
  const fields = [];
  let field = '';
  let quoted = false;
  for (let i = 0; i < line.length; i++) {
    const char = line[i];
    if (char === '"') {
      if (quoted && line[i + 1] === '"') {
        field += '"';
        i++;
      } else {
        quoted = !quoted;
      }
    } else if (char === ',' && !quoted) {
      fields.push(field.trim());
      field = '';
    } else {
      field += char;
    }
  }
  fields.push(field.trim());
  return fields;
}

function tokenSymbols() {
  const symbols = new Map();
  const lines = fs.readFileSync(TOKENS_FILE, 'utf8').split(/\r?\n/).slice(1);
  for (const line of lines) {
    if (!line.trim()) continue;
    const fields = parseCsvLine(line);
    if (fields.length >= 3 && !symbols.has(fields[0])) symbols.set(fields[0], fields[2]);
  }
  return symbols;
}

function short(address) {
  return `${address.slice(0, 4)}…${address.slice(-4)}`;
}

function slug(value) {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
}

function yaml(value) {
  return JSON.stringify(String(value));
}

async function discoverMarkets() {
  const entries = await rpc('getProgramAccounts', [
    SOLFI_PROGRAM,
    {
      encoding: 'base64',
      dataSlice: { offset: 0, length: 184 },
      filters: [{ dataSize: MARKET_SIZE }],
    },
  ]);

  const markets = entries.map(entry => {
    const data = Buffer.from(entry.account.data[0], 'base64');
    return {
      address: entry.pubkey,
      oracle: pubkey(data, 24),
      baseVault: pubkey(data, 120),
      quoteVault: pubkey(data, 152),
    };
  });

  const vaultAddresses = [
    ...new Set(markets.flatMap(market => [market.baseVault, market.quoteVault])),
  ].filter(address => address !== ZERO_PUBKEY);
  const vaults = new Map();
  for (let i = 0; i < vaultAddresses.length; i += 100) {
    const addresses = vaultAddresses.slice(i, i + 100);
    const result = await rpc('getMultipleAccounts', [addresses, { encoding: 'base64' }]);
    addresses.forEach((address, index) => {
      const account = result.value[index];
      if (!account) return;
      const data = Buffer.from(account.data[0], 'base64');
      if (data.length < 32) return;
      vaults.set(address, {
        mint: pubkey(data, 0),
      });
    });
  }

  const symbols = tokenSymbols();
  for (const market of markets) {
    const base = vaults.get(market.baseVault);
    const quote = vaults.get(market.quoteVault);
    market.baseMint = base?.mint;
    market.quoteMint = quote?.mint;
    market.baseSymbol = symbols.get(base?.mint) || (base?.mint ? short(base.mint) : 'Base');
    market.quoteSymbol = symbols.get(quote?.mint) || (quote?.mint ? short(quote.mint) : 'Quote');
    market.pair = `${market.baseSymbol} / ${market.quoteSymbol}`;
  }

  markets.sort((a, b) => {
    const aPriority = DEFAULT_MARKETS.indexOf(a.address);
    const bPriority = DEFAULT_MARKETS.indexOf(b.address);
    if (aPriority !== -1 || bPriority !== -1) {
      if (aPriority === -1) return 1;
      if (bPriority === -1) return -1;
      return aPriority - bPriority;
    }
    return a.pair.localeCompare(b.pair) || a.address.localeCompare(b.address);
  });
  return markets;
}

function metadataLines(market, extra = {}) {
  const metadata = {
    market: market.address,
    pair: market.pair.replaceAll(' ', ''),
    oracle: market.oracle,
    base_vault: market.baseVault,
    quote_vault: market.quoteVault,
    base_mint: market.baseMint,
    quote_mint: market.quoteMint,
    ...extra,
  };
  return Object.entries(metadata)
    .filter(([, value]) => value !== undefined)
    .map(([key, value]) => `          ${key}: ${yaml(value)}`)
    .join('\n');
}

function optionId(market, suffix = '') {
  const pair = slug(`${market.baseSymbol}-${market.quoteSymbol}`) || 'market';
  return `${pair}-${market.address.slice(0, 6)}${suffix}`;
}

function marketOptions(markets) {
  return markets
    .map(market => `      - id: ${yaml(optionId(market))}
        label: ${yaml(market.pair)}
        description: ${yaml(`SolFi market ${short(market.address)}; availability depends on its current vault balances.`)}
        value: ${yaml(market.address)}
        metadata:
${metadataLines(market)}`)
    .join('\n');
}

function oracleOptions(markets) {
  return markets
    .filter(market => market.oracle !== ZERO_PUBKEY)
    .map(market => `      - id: ${yaml(optionId(market))}
        label: ${yaml(market.pair)}
        description: ${yaml(`Oracle used by SolFi market ${short(market.address)}.`)}
        value: ${yaml(market.oracle)}
        metadata:
${metadataLines(market)}`)
    .join('\n');
}

function vaultOptions(markets) {
  return markets
    .flatMap(market => [
      {
        market,
        id: optionId(market, '-base'),
        label: `${market.pair} — ${market.baseSymbol} vault`,
        description: `${market.baseSymbol} inventory for SolFi market ${short(market.address)}.`,
        value: market.baseVault,
        side: 'base',
      },
      {
        market,
        id: optionId(market, '-quote'),
        label: `${market.pair} — ${market.quoteSymbol} vault`,
        description: `${market.quoteSymbol} inventory for SolFi market ${short(market.address)}.`,
        value: market.quoteVault,
        side: 'quote',
      },
    ])
    .filter(option => option.value !== ZERO_PUBKEY)
    .map(option => {
      return `      - id: ${yaml(option.id)}
        label: ${yaml(option.label)}
        description: ${yaml(option.description)}
        value: ${yaml(option.value)}
        metadata:
${metadataLines(option.market, { side: option.side })}`;
    })
    .join('\n');
}

function replaceConstants(file, label, description, options) {
  const content = fs.readFileSync(file, 'utf8');
  const start = content.indexOf('\nconstants:');
  const end = content.indexOf('\ntemplates:');
  if (start === -1 || end === -1 || start > end) {
    throw new Error(`Could not locate constants section in ${file}`);
  }
  const constants = `
# Generated by generate-markets.js from every ${MARKET_SIZE}-byte SolFi market; liquidity is not filtered.
constants:
  market:
    label: ${yaml(label)}
    description: ${yaml(description)}
    options:
${options}
`;
  fs.writeFileSync(file, content.slice(0, start) + constants + content.slice(end));
}

async function main() {
  const markets = await discoverMarkets();
  if (markets.length === 0) throw new Error('SolFi returned no market accounts');

  replaceConstants(
    FILES.market,
    'SolFi market',
    'Choose a generated SolFi market or enter a market address manually in Studio.',
    marketOptions(markets)
  );
  replaceConstants(
    FILES.oracle,
    'SolFi market',
    'Choose a generated SolFi market or enter its oracle address manually in Studio.',
    oracleOptions(markets)
  );
  replaceConstants(
    FILES.vault,
    'SolFi market vault',
    'Choose a generated market vault or enter a vault address manually in Studio.',
    vaultOptions(markets)
  );

  console.log(`Updated all SolFi selectors with ${markets.length} markets and ${markets.length * 2} vault choices.`);
}

main().catch(error => {
  console.error(`Failed to generate SolFi markets: ${error.message}`);
  process.exit(1);
});
