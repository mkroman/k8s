'use strict';

const fs = require('fs');
const path = require('path');

let buf = fs.readFileSync('dashboards.json');
let json = JSON.parse(buf);

for (const index in json) {
  for (const [name, source] of Object.entries(json[index])) {
    fs.writeFileSync(`dashboards/${name}`, source);
    console.log(`${path.basename(name, '.json')}:`);
    console.log(`  file: dashboards/${name}`);
  }
}
