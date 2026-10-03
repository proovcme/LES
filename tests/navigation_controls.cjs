// Execute the actual navigation module without a browser or visual assertions.
const vm = require('node:vm');
const fs = require('node:fs');
const assert = require('node:assert/strict');
const source = fs.readFileSync('qdrant_visualizer/navigation.js', 'utf8');
const saved = new Map();
function boot(url, locked=false) {
  let current = new URL(url, 'http://localhost:9000');
  const events = {}, delivered = [], assigned = [];
  const context = {
    URL, JSON,
    location: {get origin(){return current.origin}, get href(){return current.href},
      get pathname(){return current.pathname}, assign(value){assigned.push(value);current=new URL(value,current)}},
    history: {pushState(_,__,value){current=new URL(value,current)}, replaceState(_,__,value){current=new URL(value,current)}},
    sessionStorage: {getItem(key){if(locked)throw Error('locked');return saved.get(key)},
      setItem(key,value){if(locked)throw Error('locked');saved.set(key,value)}},
    document: {addEventListener(name,handler){events[name]=handler}},
    window: {addEventListener(name,handler){events[name]=handler},emitEvent(name,value){delivered.push([name,value])}},
  };
  vm.runInNewContext(source, context);
  return {nav:context.window.lesNavigation, events, delivered, assigned, context};
}
let app = boot('/classic?tab=chat');
app.nav.record('/classic?tab=data');
app.nav.record('/classic?tab=mail');
app.nav.back();
assert.equal(app.context.location.href, 'http://localhost:9000/classic?tab=data');
assert.equal(app.delivered.at(-1)[1].tab, 'data');
assert.equal(app.assigned.length, 0); // no page reconstruction when returning within workspace
app = boot('/les/classic?tab=models');
app.nav.record('/les/classic?tab=profiles');
app.nav.back();
assert.equal(app.delivered.at(-1)[1].tab, 'models');
app.nav.back();
assert.equal(app.assigned.at(-1), '/classic?tab=data');
for(const value of ['https://evil.example/classic','javascript:alert(1)','//evil.example/classic','/api/delete','http://user@localhost:9000/classic'])
  assert.equal(app.nav.record(value), false);
assert.equal(app.nav.canonical('/classic?tab=unknown&secret=ignored'), '/classic?tab=chat');
app = boot('/classic?tab=chat', true);
app.nav.record('/classic?tab=data');app.nav.back();
assert.equal(app.delivered.at(-1)[1].tab,'chat');
for(let i=0;i<100;i++)app.nav.record('/classic?tab='+(i%2?'chat':'data'));
app = boot('/classic?tab=chat');
for(let i=0;i<100;i++)app.nav.record('/classic?tab='+(i%2?'chat':'data'));
assert.ok(JSON.parse(saved.get('les.navigation.v1')).length<=40);
app.context.history.replaceState(null,'','/classic?tab=history');app.events.popstate();
assert.equal(app.delivered.at(-1)[1].tab,'history');
console.log('navigation: same-page, cross-page, storage denial, safety, bounds and browser Back passed');
