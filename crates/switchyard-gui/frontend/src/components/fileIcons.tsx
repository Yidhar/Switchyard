import React from 'react';
import {
  FileText,
  FileCode,
  FileCode2,
  FileJson,
  FileLock,
  FileTerminal,
  FileImage,
  FileVideo,
  FileAudio,
  FileArchive,
  Folder,
  FolderOpen,
  FileType,
  Settings,
  Database,
  BookOpen,
  Bug,
  ScrollText,
  Hexagon,
} from 'lucide-react';

/// Pick a lucide icon + tint color for a file entry, mirroring how
/// VS Code's Material Icon Theme distinguishes file types at a glance.
/// The color set leans on Tailwind-ish hex values so it matches the
/// rest of the GUI's palette without pulling in a new design token.
///
/// Lookups go in priority order:
///   1. Exact filename match (Cargo.toml, package.json, README.md, …)
///   2. Filename starts with a known prefix (Dockerfile.dev → Dockerfile)
///   3. Extension match (.rs, .ts, …)
///   4. Default text-file fallback
export interface FileIconSpec {
  icon: React.ReactNode;
  color: string;
}

const SIZE = 14;

/// Common code-color palette. Named so the call sites read as intent
/// rather than hex noise.
const RUST_ORANGE = '#dea584';
const TS_BLUE = '#3b82f6';
const JS_YELLOW = '#facc15';
const PY_BLUE = '#3776ab';
const PY_YELLOW = '#ffd43b';
const GO_CYAN = '#06b6d4';
const HTML_ORANGE = '#fb923c';
const CSS_BLUE = '#60a5fa';
const MD_BLUE = '#7dd3fc';
const JSON_YELLOW = '#facc15';
const TOML_ORANGE = '#fb923c';
const YAML_PURPLE = '#a78bfa';
const SHELL_GRAY = '#a3a3a3';
const LOCK_GRAY = '#9ca3af';
const IMAGE_PURPLE = '#c084fc';
const VIDEO_PINK = '#f472b6';
const AUDIO_PINK = '#fb7185';
const ARCHIVE_BROWN = '#d4a373';
const SQL_AMBER = '#fbbf24';
const DOCKER_BLUE = '#60a5fa';
const GIT_ORANGE = '#f97316';
const CONFIG_GRAY = '#9ca3af';
const DB_GREEN = '#34d399';

const EXACT_FILENAMES: Record<string, FileIconSpec> = {
  'cargo.toml': { icon: <Hexagon size={SIZE} />, color: RUST_ORANGE },
  'cargo.lock': { icon: <FileLock size={SIZE} />, color: RUST_ORANGE },
  'package.json': { icon: <FileJson size={SIZE} />, color: '#cb3837' }, // npm red
  'package-lock.json': { icon: <FileLock size={SIZE} />, color: '#cb3837' },
  'pnpm-lock.yaml': { icon: <FileLock size={SIZE} />, color: '#f69220' },
  'yarn.lock': { icon: <FileLock size={SIZE} />, color: '#2188b6' },
  'bun.lockb': { icon: <FileLock size={SIZE} />, color: '#fbf0df' },
  'tsconfig.json': { icon: <Settings size={SIZE} />, color: TS_BLUE },
  'jsconfig.json': { icon: <Settings size={SIZE} />, color: JS_YELLOW },
  'tauri.conf.json': { icon: <Settings size={SIZE} />, color: '#ffc131' },
  'readme.md': { icon: <BookOpen size={SIZE} />, color: MD_BLUE },
  'license': { icon: <ScrollText size={SIZE} />, color: '#fcd34d' },
  'license.md': { icon: <ScrollText size={SIZE} />, color: '#fcd34d' },
  'license.txt': { icon: <ScrollText size={SIZE} />, color: '#fcd34d' },
  '.gitignore': { icon: <FileText size={SIZE} />, color: GIT_ORANGE },
  '.gitattributes': { icon: <FileText size={SIZE} />, color: GIT_ORANGE },
  '.tauriignore': { icon: <FileText size={SIZE} />, color: '#ffc131' },
  '.npmrc': { icon: <Settings size={SIZE} />, color: '#cb3837' },
  '.eslintrc': { icon: <Settings size={SIZE} />, color: '#4b32c3' },
  '.eslintrc.json': { icon: <Settings size={SIZE} />, color: '#4b32c3' },
  '.eslintrc.js': { icon: <Settings size={SIZE} />, color: '#4b32c3' },
  '.prettierrc': { icon: <Settings size={SIZE} />, color: '#ff6b6b' },
  '.editorconfig': { icon: <Settings size={SIZE} />, color: CONFIG_GRAY },
  'dockerfile': { icon: <FileCode size={SIZE} />, color: DOCKER_BLUE },
  'makefile': { icon: <FileCode size={SIZE} />, color: '#ef4444' },
  'changelog.md': { icon: <ScrollText size={SIZE} />, color: MD_BLUE },
  'roadmap.md': { icon: <ScrollText size={SIZE} />, color: MD_BLUE },
};

/// Filenames that start with these (case-insensitive) match by prefix —
/// handles `Dockerfile.dev`, `Makefile.local`, etc.
const FILENAME_PREFIXES: Array<[string, FileIconSpec]> = [
  ['dockerfile', { icon: <FileCode size={SIZE} />, color: DOCKER_BLUE }],
  ['makefile', { icon: <FileCode size={SIZE} />, color: '#ef4444' }],
];

const EXTENSIONS: Record<string, FileIconSpec> = {
  // Rust
  rs: { icon: <FileCode size={SIZE} />, color: RUST_ORANGE },
  toml: { icon: <FileCode size={SIZE} />, color: TOML_ORANGE },
  // JS family
  js: { icon: <FileCode size={SIZE} />, color: JS_YELLOW },
  jsx: { icon: <FileCode size={SIZE} />, color: JS_YELLOW },
  mjs: { icon: <FileCode size={SIZE} />, color: JS_YELLOW },
  cjs: { icon: <FileCode size={SIZE} />, color: JS_YELLOW },
  ts: { icon: <FileCode size={SIZE} />, color: TS_BLUE },
  tsx: { icon: <FileCode size={SIZE} />, color: TS_BLUE },
  // Web
  html: { icon: <FileCode size={SIZE} />, color: HTML_ORANGE },
  htm: { icon: <FileCode size={SIZE} />, color: HTML_ORANGE },
  vue: { icon: <FileCode size={SIZE} />, color: '#42b883' },
  svelte: { icon: <FileCode size={SIZE} />, color: '#ff3e00' },
  css: { icon: <FileCode size={SIZE} />, color: CSS_BLUE },
  scss: { icon: <FileCode size={SIZE} />, color: '#ec4899' },
  sass: { icon: <FileCode size={SIZE} />, color: '#ec4899' },
  less: { icon: <FileCode size={SIZE} />, color: CSS_BLUE },
  // Python
  py: { icon: <FileCode size={SIZE} />, color: PY_BLUE },
  pyi: { icon: <FileCode size={SIZE} />, color: PY_YELLOW },
  ipynb: { icon: <FileCode size={SIZE} />, color: '#f37626' },
  // JVM / .NET
  java: { icon: <FileCode size={SIZE} />, color: '#ea580c' },
  kt: { icon: <FileCode size={SIZE} />, color: '#a855f7' },
  kts: { icon: <FileCode size={SIZE} />, color: '#a855f7' },
  scala: { icon: <FileCode size={SIZE} />, color: '#dc2626' },
  cs: { icon: <FileCode size={SIZE} />, color: '#9333ea' },
  fs: { icon: <FileCode size={SIZE} />, color: '#3b82f6' },
  // Systems
  c: { icon: <FileCode size={SIZE} />, color: '#3b82f6' },
  h: { icon: <FileCode size={SIZE} />, color: '#a78bfa' },
  cpp: { icon: <FileCode size={SIZE} />, color: '#3b82f6' },
  cc: { icon: <FileCode size={SIZE} />, color: '#3b82f6' },
  hpp: { icon: <FileCode size={SIZE} />, color: '#a78bfa' },
  go: { icon: <FileCode size={SIZE} />, color: GO_CYAN },
  // Other compiled
  swift: { icon: <FileCode size={SIZE} />, color: '#fa7343' },
  rb: { icon: <FileCode size={SIZE} />, color: '#cc342d' },
  php: { icon: <FileCode size={SIZE} />, color: '#7377ad' },
  // Scripts
  sh: { icon: <FileTerminal size={SIZE} />, color: SHELL_GRAY },
  bash: { icon: <FileTerminal size={SIZE} />, color: SHELL_GRAY },
  zsh: { icon: <FileTerminal size={SIZE} />, color: SHELL_GRAY },
  fish: { icon: <FileTerminal size={SIZE} />, color: SHELL_GRAY },
  ps1: { icon: <FileTerminal size={SIZE} />, color: '#012456' },
  bat: { icon: <FileTerminal size={SIZE} />, color: SHELL_GRAY },
  cmd: { icon: <FileTerminal size={SIZE} />, color: SHELL_GRAY },
  // Data
  json: { icon: <FileJson size={SIZE} />, color: JSON_YELLOW },
  jsonc: { icon: <FileJson size={SIZE} />, color: JSON_YELLOW },
  yaml: { icon: <FileCode size={SIZE} />, color: YAML_PURPLE },
  yml: { icon: <FileCode size={SIZE} />, color: YAML_PURPLE },
  xml: { icon: <FileCode size={SIZE} />, color: '#fb923c' },
  csv: { icon: <FileText size={SIZE} />, color: DB_GREEN },
  tsv: { icon: <FileText size={SIZE} />, color: DB_GREEN },
  // Docs
  md: { icon: <FileText size={SIZE} />, color: MD_BLUE },
  markdown: { icon: <FileText size={SIZE} />, color: MD_BLUE },
  mdx: { icon: <FileText size={SIZE} />, color: MD_BLUE },
  rst: { icon: <FileText size={SIZE} />, color: MD_BLUE },
  txt: { icon: <FileText size={SIZE} />, color: 'var(--text-muted)' },
  // Lock / config
  lock: { icon: <FileLock size={SIZE} />, color: LOCK_GRAY },
  ini: { icon: <Settings size={SIZE} />, color: CONFIG_GRAY },
  conf: { icon: <Settings size={SIZE} />, color: CONFIG_GRAY },
  cfg: { icon: <Settings size={SIZE} />, color: CONFIG_GRAY },
  env: { icon: <Settings size={SIZE} />, color: '#facc15' },
  // Database
  sql: { icon: <Database size={SIZE} />, color: SQL_AMBER },
  sqlite: { icon: <Database size={SIZE} />, color: '#0ea5e9' },
  sqlite3: { icon: <Database size={SIZE} />, color: '#0ea5e9' },
  db: { icon: <Database size={SIZE} />, color: '#0ea5e9' },
  // Images
  png: { icon: <FileImage size={SIZE} />, color: IMAGE_PURPLE },
  jpg: { icon: <FileImage size={SIZE} />, color: IMAGE_PURPLE },
  jpeg: { icon: <FileImage size={SIZE} />, color: IMAGE_PURPLE },
  gif: { icon: <FileImage size={SIZE} />, color: IMAGE_PURPLE },
  webp: { icon: <FileImage size={SIZE} />, color: IMAGE_PURPLE },
  bmp: { icon: <FileImage size={SIZE} />, color: IMAGE_PURPLE },
  ico: { icon: <FileImage size={SIZE} />, color: IMAGE_PURPLE },
  svg: { icon: <FileImage size={SIZE} />, color: '#facc15' },
  // Video / audio
  mp4: { icon: <FileVideo size={SIZE} />, color: VIDEO_PINK },
  webm: { icon: <FileVideo size={SIZE} />, color: VIDEO_PINK },
  mov: { icon: <FileVideo size={SIZE} />, color: VIDEO_PINK },
  mkv: { icon: <FileVideo size={SIZE} />, color: VIDEO_PINK },
  mp3: { icon: <FileAudio size={SIZE} />, color: AUDIO_PINK },
  wav: { icon: <FileAudio size={SIZE} />, color: AUDIO_PINK },
  flac: { icon: <FileAudio size={SIZE} />, color: AUDIO_PINK },
  ogg: { icon: <FileAudio size={SIZE} />, color: AUDIO_PINK },
  // Archives
  zip: { icon: <FileArchive size={SIZE} />, color: ARCHIVE_BROWN },
  tar: { icon: <FileArchive size={SIZE} />, color: ARCHIVE_BROWN },
  gz: { icon: <FileArchive size={SIZE} />, color: ARCHIVE_BROWN },
  '7z': { icon: <FileArchive size={SIZE} />, color: ARCHIVE_BROWN },
  rar: { icon: <FileArchive size={SIZE} />, color: ARCHIVE_BROWN },
  // Fonts
  woff: { icon: <FileType size={SIZE} />, color: '#a78bfa' },
  woff2: { icon: <FileType size={SIZE} />, color: '#a78bfa' },
  ttf: { icon: <FileType size={SIZE} />, color: '#a78bfa' },
  otf: { icon: <FileType size={SIZE} />, color: '#a78bfa' },
  // Misc
  log: { icon: <ScrollText size={SIZE} />, color: 'var(--text-muted)' },
  patch: { icon: <FileCode2 size={SIZE} />, color: GIT_ORANGE },
  diff: { icon: <FileCode2 size={SIZE} />, color: GIT_ORANGE },
};

export function iconForFile(name: string): FileIconSpec {
  if (!name) return { icon: <FileText size={SIZE} />, color: 'var(--text-muted)' };
  const lower = name.toLowerCase();
  // Exact filename — most specific signal (Cargo.toml beats *.toml).
  if (EXACT_FILENAMES[lower]) return EXACT_FILENAMES[lower];
  // Prefix match — Dockerfile.dev → Dockerfile.
  for (const [prefix, spec] of FILENAME_PREFIXES) {
    if (lower.startsWith(prefix)) return spec;
  }
  // Extension fallback.
  const dotIdx = lower.lastIndexOf('.');
  if (dotIdx > 0 && dotIdx < lower.length - 1) {
    const ext = lower.slice(dotIdx + 1);
    if (EXTENSIONS[ext]) return EXTENSIONS[ext];
  }
  // Bug-related stems read better with a different icon than plain text.
  if (lower.includes('todo') || lower.includes('bug')) {
    return { icon: <Bug size={SIZE} />, color: '#fb7185' };
  }
  return { icon: <FileText size={SIZE} />, color: 'var(--text-muted)' };
}

/// Folder icon variant — VS Code uses a different glyph for a few
/// well-known folder names. We hit the common ones to keep the tree
/// scannable.
const FOLDER_NAMES: Record<string, { color: string }> = {
  src: { color: TS_BLUE },
  crates: { color: RUST_ORANGE },
  tests: { color: '#34d399' },
  test: { color: '#34d399' },
  __tests__: { color: '#34d399' },
  docs: { color: MD_BLUE },
  doc: { color: MD_BLUE },
  '.git': { color: GIT_ORANGE },
  '.github': { color: '#a78bfa' },
  node_modules: { color: '#404040' },
  target: { color: '#404040' },
  dist: { color: '#404040' },
  build: { color: '#404040' },
  '.next': { color: '#404040' },
  '.nuxt': { color: '#404040' },
  '.cache': { color: '#404040' },
  '.vscode': { color: TS_BLUE },
  '.idea': { color: '#fbbf24' },
  '.switchyard': { color: '#6366f1' },
  scripts: { color: SHELL_GRAY },
  assets: { color: IMAGE_PURPLE },
  public: { color: IMAGE_PURPLE },
  images: { color: IMAGE_PURPLE },
  img: { color: IMAGE_PURPLE },
  components: { color: TS_BLUE },
  pages: { color: TS_BLUE },
  api: { color: '#34d399' },
  config: { color: CONFIG_GRAY },
  hooks: { color: '#34d399' },
  utils: { color: CONFIG_GRAY },
  lib: { color: TS_BLUE },
};

export function iconForFolder(name: string, expanded: boolean): FileIconSpec {
  const lower = name.toLowerCase();
  const spec = FOLDER_NAMES[lower];
  const color = spec?.color ?? '#94a3b8';
  return {
    icon: expanded ? <FolderOpen size={SIZE} /> : <Folder size={SIZE} />,
    color,
  };
}
