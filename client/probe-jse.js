const fs = require('fs');
const j = fs.readFileSync('node_modules/vanilla-jsoneditor/index.js', 'utf8');
for (const k of ['<style', 'jse-root', 'adoptedStyleSheets', 'createElement("style']) {
  const i = j.indexOf(k);
  console.log(k, '->', i === -1 ? 'NOT FOUND' : JSON.stringify(j.slice(Math.max(0, i - 120), i + 80).replace(/\s+/g, ' ')));
}
const rm = fs.readFileSync('node_modules/vanilla-jsoneditor/README.md', 'utf8');
let i = rm.indexOf('createJSONEditor({');
console.log('===EXAMPLE===');
console.log(rm.slice(i, i + 900));
