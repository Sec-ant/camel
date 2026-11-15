import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {Window} from 'happy-dom';

async function until(predicate) {
  for (let i = 0; i < 100; i++) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  throw new Error('UI did not reach expected state');
}

test('reader toggles layers and persists edited dialogue', async () => {
  const window = new Window({url:'http://localhost/session/'});
  window.document.body.innerHTML = '<div id="app"></div>';
  window.scrollTo = () => {};
  const png = 'data:image/png;base64,iVBORw0KGgo=';
  const patch = {id:7,bounds:{x:10,y:10,width:20,height:20},image:png};
  const page = {page:0,total:1,name:'page.png',width:100,height:100,original:png,erasures:[patch],texts:[patch],issues:[],dialogue:[{id:7,source:'HELLO',translation:'你好',bounds:patch.bounds}]};
  const edits = [];
  window.fetch = async (url, options) => {
    if (url === 'api/edit') {
      const edit = JSON.parse(options.body); edits.push(edit);
      page.dialogue[0].translation = edit.translation;
      return {ok:true};
    }
    assert.equal(url,'api/page/0');
    return {ok:true,json:async()=>structuredClone(page)};
  };
  window.eval(await readFile(new URL('../src/project/reader.js', import.meta.url),'utf8'));
  await until(() => window.document.querySelector('textarea'));
  assert.equal(window.document.querySelectorAll('img.patch').length, 2);
  window.document.querySelector('input[type=checkbox]').click();
  await until(() => window.document.querySelectorAll('img.patch').length === 1);
  const input = window.document.querySelector('textarea');
  const setter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype,'value').set;
  setter.call(input,'再见');
  input.dispatchEvent(new window.Event('input',{bubbles:true}));
  await until(() => window.document.body.textContent.includes('1 处未保存'));
  const button = [...window.document.querySelectorAll('button')].find(b => b.textContent === '保存');
  button.click();
  await until(() => edits.length === 1 && window.document.body.textContent.includes('已保存'));
  assert.deepEqual(edits[0],{page:0,id:7,expected:'你好',translation:'再见'});
  window.happyDOM.abort();
});

test('a saved edit remains the baseline if refresh fails', async () => {
  const window = new Window({url:'http://localhost/session/'});
  window.document.body.innerHTML = '<div id="app"></div>';
  window.scrollTo = () => {};
  const bounds = {x:10,y:10,width:20,height:20};
  const page = {page:0,total:1,name:'page.png',width:100,height:100,original:'data:image/png;base64,iVBORw0KGgo=',erasures:[],texts:[],issues:[],dialogue:[{id:7,source:'HELLO',translation:'你好',bounds}]};
  const edits = [];
  let reads = 0;
  window.fetch = async (url, options) => {
    if (url === 'api/edit') {
      const edit = JSON.parse(options.body); edits.push(edit);
      page.dialogue[0].translation = edit.translation;
      return {ok:true};
    }
    reads++;
    if (reads === 2) return {ok:false,text:async()=> '连接中断'};
    return {ok:true,json:async()=>structuredClone(page)};
  };
  window.eval(await readFile(new URL('../src/project/reader.js', import.meta.url),'utf8'));
  await until(() => window.document.querySelector('textarea'));
  const input = window.document.querySelector('textarea');
  const setter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype,'value').set;
  const edit = value => { setter.call(input,value); input.dispatchEvent(new window.Event('input',{bubbles:true})); };
  edit('再见');
  await until(() => window.document.body.textContent.includes('1 处未保存'));
  [...window.document.querySelectorAll('button')].find(b => b.textContent === '保存').click();
  await until(() => window.document.body.textContent.includes('译文已保存，但刷新失败'));
  await until(() => !input.disabled);
  edit('再会');
  await until(() => window.document.body.textContent.includes('1 处未保存'));
  [...window.document.querySelectorAll('button')].find(b => b.textContent === '保存').click();
  await until(() => edits.length === 2);
  assert.equal(edits[1].expected, '再见');
  window.happyDOM.abort();
});
