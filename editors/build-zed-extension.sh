#!/bin/sh
# Copies the Kuana extension's language files and snippets into the Zed extension and pins its
# grammar to the last commit that changed tree-sitter-oxie (Zed builds the grammar from the repository).
set -e
cd "$(dirname "$0")"
rm -rf zed-oxie/languages zed-oxie/snippets
mkdir -p zed-oxie/languages/oxie zed-oxie/snippets
cp kuana-oxie/languages/oxie/config.toml kuana-oxie/languages/oxie/tasks.json zed-oxie/languages/oxie/
for q in kuana-oxie/languages/oxie/*.scm; do
    # Zed's themes color escapes as string.escape, and Zed has no parameter text objects.
    sed -E -e 's/@escape([^a-zA-Z0-9_.]|$)/@string.escape\1/g' -e '/@parameter\./d' "$q" > "zed-oxie/languages/oxie/$(basename "$q")"
done
cp kuana-oxie/snippets/oxie.json zed-oxie/snippets/
rev=$(git log -1 --format=%H -- tree-sitter-oxie)
sed -E "s/^rev = \".*\"/rev = \"$rev\"/" zed-oxie/extension.toml > zed-oxie/extension.toml.new
mv zed-oxie/extension.toml.new zed-oxie/extension.toml
if [ -n "$(git status --porcelain -- tree-sitter-oxie)" ]; then
    echo "warning: tree-sitter-oxie has uncommitted changes; Zed builds the grammar at $rev." >&2
    echo "Commit them, then run this script again." >&2
fi
echo "Zed extension updated: $(pwd)/zed-oxie (grammar at $rev)"
