#!/bin/sh
# Regenerates the tree-sitter parser and copies it with the queries into the Kuana extension, then
# updates the Zed extension from it.
set -e
cd "$(dirname "$0")/tree-sitter-oxie"
[ -x node_modules/.bin/tree-sitter ] || npm install --no-audit --no-fund
./node_modules/.bin/tree-sitter generate
cd ..
rm -rf kuana-oxie/grammars/oxie
mkdir -p kuana-oxie/grammars/oxie
cp -R tree-sitter-oxie/src kuana-oxie/grammars/oxie/src
rm -f kuana-oxie/grammars/oxie/src/grammar.json kuana-oxie/grammars/oxie/src/node-types.json
for q in highlights outline indents brackets textobjects runnables injections; do
    cp "tree-sitter-oxie/queries/$q.scm" "kuana-oxie/languages/oxie/$q.scm"
done
echo "Kuana extension updated: $(pwd)/kuana-oxie"
./build-zed-extension.sh
