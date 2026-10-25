// Generates the site's pages in src/content/docs from docs/*.md, CHANGELOG.md, and the guide
// `oxie guide` prints (crates/oxie-cli/guide/writing-oxie.md), so the
// Markdown in docs/ stays the only copy of the documentation. Each ```oxie example gets tabs
// with the Rust that `oxie emit` produces for it and the output recorded by the doc-example test
// (src/data/doc-outputs.json, written by `OXIE_BLESS=1 cargo test -p oxie --test e2e
// documentation_examples_run`).

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const website = path.join(repo, 'website');
const outDir = path.join(website, 'src/content/docs');
const partsDir = path.join(website, 'src/generated');
const outputsFile = path.join(website, 'src/data/doc-outputs.json');
const github = 'https://github.com/oxie-lang/oxie';
/** The guide `oxie guide` prints, built into the binary; the site shows it as Writing Oxie. */
const writingGuide = path.join(repo, 'crates/oxie-cli/guide/writing-oxie.md');

/** Sidebar groups; every file in docs/ must appear here (writing-oxie is the guide in the crate). */
export const groups = [
	{ label: 'Learn', pages: ['guide', 'cheatsheet'] },
	{ label: 'AI coding agents', pages: ['ai-agents', 'writing-oxie'] },
	{ label: 'Language', pages: ['reference', 'errors'] },
	{ label: 'Using Oxie', pages: ['rust-interop', 'tooling', 'performance'] },
	{ label: 'Project', pages: ['internals', 'changelog'] },
];

export const sidebar = groups.map((g) => ({ label: g.label, items: g.pages }));

/** Files the pages are generated from: the dev server regenerates the site when they change. */
export function sources() {
	const docs = fs.readdirSync(path.join(repo, 'docs')).filter((f) => f.endsWith('.md'));
	return [...docs.map((f) => path.join(repo, 'docs', f)), writingGuide, path.join(repo, 'CHANGELOG.md'), path.join(repo, 'README.md'), outputsFile];
}

let oxieBin;

/** The `oxie` binary of this checkout (built if needed), or `$OXIE_BIN`. */
function oxie() {
	if (oxieBin) return oxieBin;
	if (process.env.OXIE_BIN) return (oxieBin = process.env.OXIE_BIN);
	try {
		execFileSync('cargo', ['build', '-p', 'oxie', '--quiet'], { cwd: repo, stdio: 'inherit' });
	} catch (e) {
		throw new Error(`building oxie failed (${e.message}); the docs need Rust's cargo, or OXIE_BIN set to an oxie binary`);
	}
	const exe = process.platform === 'win32' ? 'oxie.exe' : 'oxie';
	return (oxieBin = path.join(process.env.CARGO_TARGET_DIR ?? path.join(repo, 'target'), 'debug', exe));
}

const emitted = new Map();
let tmpDir;

/** The Rust `oxie emit` generates for an example, without the runtime module header; null if
 * the example is a fragment that doesn't compile on its own. */
function emit(source) {
	if (emitted.has(source)) return emitted.get(source);
	tmpDir ??= fs.mkdtempSync(path.join(os.tmpdir(), 'oxie-docs-'));
	const file = path.join(tmpDir, 'example.oxi');
	fs.writeFileSync(file, source);
	// outside the try below: a failed build of oxie stops the site build
	const bin = oxie();
	let rust = null;
	try {
		const out = execFileSync(bin, ['emit', file], { cwd: tmpDir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'], env: { ...process.env, NO_COLOR: '1' } });
		const lines = out.split('\n');
		const prelude = lines.findIndex((l) => l.startsWith('use crate::__oxie::prelude::*;'));
		rust = lines.slice(prelude + 1).join('\n').trim() + '\n';
	} catch (e) {
		// a non-zero exit: the example doesn't compile on its own (a fragment, or an example of an error)
		if (typeof e.status !== 'number') throw new Error(`running ${bin} failed: ${e.message}`);
	}
	emitted.set(source, rust);
	return rust;
}

function recordedOutputs() {
	try {
		return new Map(JSON.parse(fs.readFileSync(outputsFile, 'utf8')).map((o) => [o.source, o.output]));
	} catch {
		return new Map();
	}
}

function fence(lang, code) {
	const ticks = code.includes('```') ? '````' : '```';
	return `${ticks}${lang}\n${code.endsWith('\n') ? code : code + '\n'}${ticks}`;
}

let exampleId = 0;

/** Tabs with the example, its Rust and its output (radio buttons, so they work without JS). */
function example(source, outputs) {
	const tabs = [['oxie', 'Oxie', fence('oxie', source)]];
	const rust = emit(source);
	if (rust) tabs.push(['rust', 'Generated Rust', `<p class="oxie-panel-note">What <code>oxie emit</code> generates for this example, produced by the current compiler.</p>\n\n${fence('rust', rust)}`]);
	const output = outputs.get(source);
	if (output) tabs.push(['output', 'Output', fence('text', output)]);
	if (tabs.length === 1) return tabs[0][2];
	const id = `ex${++exampleId}`;
	const inputs = tabs.map(([key, label], i) => `<input type="radio" name="${id}" id="${id}-${key}" value="${key}"${i === 0 ? ' checked' : ''}><label for="${id}-${key}">${label}</label>`);
	const panels = tabs.map(([key, , code]) => `<div class="oxie-panel" data-panel="${key}">\n\n${code}\n\n</div>`);
	return `<div class="oxie-example">\n<div class="oxie-tabs">${inputs.join('')}</div>\n${panels.join('\n')}\n</div>`;
}

/** Points links at site pages (`guide.md#x` → `/guide/#x`) or at files on GitHub. */
function rewriteLinks(text, srcDir) {
	return text.replace(/\]\(([^)\s]+)\)/g, (all, target) => {
		if (/^([a-z]+:|#)/.test(target)) return all;
		const [file, hash = ''] = target.split('#');
		const rel = path.relative(repo, path.resolve(srcDir, file)).replaceAll('\\', '/');
		const page = rel === 'CHANGELOG.md' ? 'changelog' : rel === 'crates/oxie-cli/guide/writing-oxie.md' ? 'writing-oxie' : rel.match(/^docs\/([\w-]+)\.md$/)?.[1];
		if (page) return `](/${page}/${hash ? '#' + hash : ''})`;
		const kind = /\.\w+$/.test(rel) ? 'blob' : 'tree';
		return `](${github}/${kind}/main/${rel}${hash ? '#' + hash : ''})`;
	});
}

/** Plain text of the first paragraph, for the page's meta description. */
function description(lines) {
	const start = lines.findIndex((l) => l.trim() && !l.startsWith('#'));
	if (start < 0) return '';
	const para = [];
	for (const l of lines.slice(start)) {
		if (!l.trim() || l.startsWith('```')) break;
		para.push(l.trim());
	}
	return para.join(' ').replace(/\[([^\]]*)\]\([^)]*\)/g, '$1').replace(/[`*]/g, '');
}

/** The Markdown of a page body: examples get their tabs, links point at the site. */
function convert(rest, srcDir, outputs) {
	const body = [];
	for (let i = 0; i < rest.length; i++) {
		if (rest[i].startsWith('```oxie')) {
			const code = [];
			while (++i < rest.length && !rest[i].startsWith('```')) code.push(rest[i] + '\n');
			body.push(example(code.join(''), outputs));
		} else if (rest[i].startsWith('```')) {
			// copy other code blocks as they are, links included
			body.push(rest[i]);
			while (++i < rest.length && !rest[i].startsWith('```')) body.push(rest[i]);
			body.push(rest[i]);
		} else {
			body.push(rewriteLinks(rest[i], srcDir));
		}
	}
	return body.join('\n');
}

function page(srcFile, slug, outputs) {
	const lines = fs.readFileSync(srcFile, 'utf8').split('\n');
	const titleLine = lines.findIndex((l) => l.startsWith('# '));
	const title = lines[titleLine].slice(2).trim();
	const rest = lines.slice(titleLine + 1);
	const rel = path.relative(repo, srcFile).replaceAll('\\', '/');
	const frontmatter = [
		'---',
		`title: ${JSON.stringify(title)}`,
		`description: ${JSON.stringify(description(rest))}`,
		`editUrl: ${github}/edit/main/${rel}`,
		'---',
		'',
		`<!-- Generated from ${rel} by website/scripts/sync-docs.mjs. Edit that file instead. -->`,
	];
	return { file: path.join(outDir, `${slug}.md`), text: frontmatter.join('\n') + '\n' + convert(rest, path.dirname(srcFile), outputs) };
}

/** A `## ` section of the README, for the home page, as src/generated/readme-<name>.md. */
function readmeSection(heading, outputs, name = heading.toLowerCase()) {
	const lines = fs.readFileSync(path.join(repo, 'README.md'), 'utf8').split('\n');
	const start = lines.indexOf(`## ${heading}`);
	if (start < 0) throw new Error(`README.md has no "## ${heading}" section for the home page`);
	const end = lines.findIndex((l, i) => i > start && l.startsWith('## '));
	const file = path.join(partsDir, `readme-${name}.md`);
	return { file, text: convert(lines.slice(start, end < 0 ? undefined : end), repo, outputs) };
}

/** The README's first example and its Rust, which the home page shows side by side. */
function homeExample() {
	const source = fs.readFileSync(path.join(repo, 'README.md'), 'utf8').match(/^```oxie\n([\s\S]*?)^```/m)[1];
	const rust = emit(source);
	if (!rust) throw new Error("the README's first example doesn't compile with `oxie emit`");
	return { file: path.join(partsDir, 'home-example.json'), text: JSON.stringify({ source, rust }, null, '\t') + '\n' };
}

/** Writes the generated pages, touching only the files whose content changed. */
export function syncDocs() {
	const outputs = recordedOutputs();
	exampleId = 0;
	const docs = fs.readdirSync(path.join(repo, 'docs')).filter((f) => f.endsWith('.md'));
	const listed = new Set(groups.flatMap((g) => g.pages));
	const missing = docs.map((f) => f.slice(0, -3)).filter((slug) => !listed.has(slug));
	if (missing.length) throw new Error(`add ${missing.map((s) => `docs/${s}.md`).join(', ')} to the sidebar groups in website/scripts/sync-docs.mjs`);
	const pages = [
		...docs.map((f) => page(path.join(repo, 'docs', f), f.slice(0, -3), outputs)),
		page(path.join(repo, 'CHANGELOG.md'), 'changelog', outputs),
		page(writingGuide, 'writing-oxie', outputs),
		readmeSection('Why', outputs),
		readmeSection('AI coding agents', outputs, 'agents'),
		readmeSection('Install', outputs),
		homeExample(),
	];
	fs.mkdirSync(partsDir, { recursive: true });
	for (const p of pages) {
		if (fs.existsSync(p.file) && fs.readFileSync(p.file, 'utf8') === p.text) continue;
		fs.writeFileSync(p.file, p.text);
	}
}

if (process.argv[1] === fileURLToPath(import.meta.url)) syncDocs();
