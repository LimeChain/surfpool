#!/usr/bin/env node

// Adds the platform packages to package.json's optionalDependencies, pinned to
// the root package version. Run in CI right before packing/publishing only.
//
// They are intentionally absent from the committed package.json: local builds
// load the binary from surfpool-sdk/, and keeping them out of the lockfile
// avoids stale stub entries that drift once the platform packages are published.

const fs = require("fs");
const path = require("path");

const PLATFORM_PACKAGES = [
  "@solana/surfpool-darwin-x64",
  "@solana/surfpool-darwin-arm64",
  "@solana/surfpool-linux-x64-gnu",
];

function main() {
  const packageJsonPath = path.resolve(__dirname, "..", "package.json");
  const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));

  packageJson.optionalDependencies = Object.fromEntries(
    PLATFORM_PACKAGES.map((packageName) => [packageName, packageJson.version]),
  );

  fs.writeFileSync(packageJsonPath, `${JSON.stringify(packageJson, null, 2)}\n`);
  console.log(
    `Injected optionalDependencies at ${packageJson.version}: ${PLATFORM_PACKAGES.join(", ")}`,
  );
}

main();
