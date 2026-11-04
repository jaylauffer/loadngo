#!/usr/bin/env node
// Runs a command and, every 5 s while it runs, records which Chrome processes are alive (headless
// or desktop, and whether each is a child of this run) into <out>/contention-during.txt, each named
// only by its kind, since another session's command line names its own project. Exits with the
// command's status.
//   node sampled.mjs <out dir> -- <command...>
import { spawn, execFileSync } from 'node:child_process';
import { writeFileSync, appendFileSync, mkdirSync } from 'node:fs';
import path from 'node:path';

const sep = process.argv.indexOf('--');
const out = path.resolve(process.argv[2]);
const cmd = process.argv.slice(sep + 1);
mkdirSync(out, { recursive: true });
const file = path.join(out, 'contention-during.txt');
writeFileSync(file, `# every 5 s during: ${cmd.map((a) => a.replace(process.env.HOME, '<home>')).join(' ')}\n`);
const child = spawn(cmd[0], cmd.slice(1), { stdio: 'inherit' });
const descendants = () => {
  const rows = execFileSync('ps', ['-axo', 'pid=,ppid=,command='], { encoding: 'utf8' }).trim().split('\n').map((l) => {
    const m = l.trim().match(/^(\d+)\s+(\d+)\s+(.*)$/);
    return { pid: +m[1], ppid: +m[2], cmd: m[3] };
  });
  const mine = new Set([child.pid]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const r of rows) if (!mine.has(r.pid) && mine.has(r.ppid)) { mine.add(r.pid); grew = true; }
  }
  return { rows, mine };
};
const sample = () => {
  const { rows, mine } = descendants();
  const browsers = rows.filter((r) => /Google Chrome\.app\/Contents\/MacOS\/Google Chrome( |$)|chrome-headless-shell|Chromium\.app/.test(r.cmd) && !/--type=/.test(r.cmd));
  const gpus = rows.filter((r) => /--type=gpu-process/.test(r.cmd));
  const desc = browsers.map((r) => `${mine.has(r.pid) ? 'this run' : 'other'} ${/--headless/.test(r.cmd) ? 'headless' : 'desktop'} pid ${r.pid}`);
  appendFileSync(file, `${new Date().toISOString()} browsers: ${desc.join('; ') || 'none'}; gpu processes: ${gpus.length}\n`);
};
sample();
const timer = setInterval(sample, 5000);
child.on('exit', (code) => {
  clearInterval(timer);
  sample();
  process.exit(code ?? 1);
});
