#!/bin/sh
# Rebuilds the lockfiles with the real package managers (network access needed).
# Each directory holds the manifest and the lockfile its tool wrote:
#   npm-v1, npm-v2, npm-v3   npm 6 / npm --lockfile-version 2 / npm (v3)
#   npm-v3-updated           npm-v3 with ms 2.1.2 and debug added
#   yarn-v1, yarn-berry      yarn 1.22, yarn 4
#   pnpm-v5, pnpm-v6, pnpm-v9  pnpm 7, 8, 10
#   cargo (with a path dependency), poetry, uv, pipenv, composer, bundler, go
set -e
cd "$(dirname "$0")"
PKG='{"name":"demo","version":"1.0.0","dependencies":{"left-pad":"1.3.0","chalk":"^4.1.2","@types/node":"20.11.0"},"devDependencies":{"ms":"2.1.3"}}'
for d in npm-v1 npm-v2 npm-v3 yarn-v1 yarn-berry pnpm-v5 pnpm-v6 pnpm-v9; do mkdir -p $d; echo "$PKG" > $d/package.json; done
mkdir -p npm-v3-updated
echo '{"name":"demo","version":"1.0.0","dependencies":{"left-pad":"1.3.0","chalk":"^4.1.2","@types/node":"20.11.0","debug":"4.3.4"},"devDependencies":{"ms":"2.1.2"}}' > npm-v3-updated/package.json
(cd npm-v1 && npx -y npm@6 install --package-lock-only --ignore-scripts)
(cd npm-v2 && npm install --package-lock-only --lockfile-version 2 --ignore-scripts)
(cd npm-v3 && npm install --package-lock-only --ignore-scripts)
(cd npm-v3-updated && npm install --package-lock-only --ignore-scripts)
(cd yarn-v1 && yarn install --ignore-scripts --silent && rm -rf node_modules)
(cd yarn-berry && touch yarn.lock && YARN_ENABLE_IMMUTABLE_INSTALLS=0 npx -y @yarnpkg/cli-dist@4.5.0 install --mode=update-lockfile; rm -rf .yarn .pnp.* )
(cd pnpm-v5 && npx -y pnpm@7 install --lockfile-only --ignore-scripts)
(cd pnpm-v6 && npx -y pnpm@8 install --lockfile-only --ignore-scripts)
(cd pnpm-v9 && pnpm install --lockfile-only --ignore-scripts)
(cd cargo && cargo generate-lockfile)
(cd poetry && poetry lock)
(cd uv && uv lock)
(cd pipenv && pipenv lock)
(cd composer && composer update --no-install --no-interaction)
(cd bundler && bundle lock)
(cd go && go mod tidy)
