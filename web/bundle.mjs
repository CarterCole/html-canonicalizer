// Assemble web/dist from web/src/canon.js, agent/agent.js and the wasm-bindgen output in
// web/pkg. Run by web/build.sh.
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const web = dirname(fileURLToPath(import.meta.url));
const root = join(web, '..');
const dist = join(web, 'dist');
mkdirSync(dist, { recursive: true });

const agent = readFileSync(join(root, 'agent/agent.js'), 'utf8')
  // the auto-install line is for the CLI's tabs only
  .replace(/\nif \(typeof window !== 'undefined'[^\n]*\n?/, '\n');
const src = readFileSync(join(web, 'src/canon.js'), 'utf8');
if (!src.includes('/*@AGENT@*/')) throw new Error('web/src/canon.js lost its /*@AGENT@*/ placeholder');
writeFileSync(join(dist, 'canon.js'), src.replace('/*@AGENT@*/', () => agent));
copyFileSync(join(web, 'pkg/canon_core.js'), join(dist, 'canon_core.js'));
copyFileSync(join(web, 'pkg/canon_core_bg.wasm'), join(dist, 'canon_core_bg.wasm'));
copyFileSync(join(web, 'pkg/canon_core.d.ts'), join(dist, 'canon_core.d.ts'));
console.log('web/dist: canon.js, canon_core.js, canon_core_bg.wasm');
