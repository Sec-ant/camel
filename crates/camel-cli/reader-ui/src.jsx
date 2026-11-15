import React, {useCallback, useEffect, useMemo, useState} from 'react';
import {createRoot} from 'react-dom/client';
import {Button} from '@astryxdesign/core';
import {Theme} from '@astryxdesign/core/theme';
import {neutralTheme} from '@astryxdesign/theme-neutral/built';
import '@astryxdesign/core/astryx.css';
import '@astryxdesign/theme-neutral/theme.css';
import './style.css';

function App() {
  const [data, setData] = useState(null);
  const [draft, setDraft] = useState({});
  const [number, setNumber] = useState('1');
  const [showErase, setShowErase] = useState(true);
  const [showText, setShowText] = useState(true);
  const [selected, setSelected] = useState(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(null);
  const [error, setError] = useState('');
  const dirty = useMemo(() => data ? data.dialogue.filter(d => (draft[d.id] ?? '') !== (d.translation ?? '')) : [], [data,draft]);
  const load = useCallback(async (page, keep = null) => {
    setLoading(true); setError('');
    try {
      const response = await fetch(`api/page/${page}`);
      if (!response.ok) throw new Error(await response.text());
      const next = await response.json();
      setData(next); setNumber(String(next.page + 1));
      setDraft(previous => Object.fromEntries(next.dialogue.map(d => [d.id, keep && keep.has(d.id) ? previous[d.id] : (d.translation ?? '')])));
      if (!keep) setSelected(null);
    } catch (e) { setError(keep ? `译文已保存，但刷新失败：${e.message}` : e.message); }
    finally { setLoading(false); }
  }, []);
  useEffect(() => { load(0); }, [load]);
  const move = page => {
    if (!data || page < 0 || page >= data.total || loading || saving !== null) return;
    if (dirty.length) { setError('请先保存或放弃本页的修改'); return; }
    load(page); window.scrollTo({top:0});
  };
  async function save(row) {
    setSaving(row.id); setError('');
    try {
      const response = await fetch('api/edit', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({page:data.page,id:row.id,expected:row.translation,translation:draft[row.id] || null})});
      if (!response.ok) throw new Error(await response.text());
      const saved = draft[row.id]?.trim() ? draft[row.id] : null;
      setData(previous => ({...previous, dialogue: previous.dialogue.map(d => d.id === row.id ? {...d, translation:saved} : d)}));
      setDraft(previous => ({...previous, [row.id]: saved ?? ''}));
      const keep = new Set(dirty.filter(d => d.id !== row.id).map(d => d.id));
      await load(data.page, keep);
    } catch (e) { setError(e.message); }
    finally { setSaving(null); }
  }
  const select = id => { setSelected(id); document.getElementById(`line-${id}`)?.scrollIntoView({behavior:'smooth',block:'nearest'}); };
  const position = r => ({left:`${100*r.x/data.width}%`,top:`${100*r.y/data.height}%`,width:`${100*r.width/data.width}%`,height:`${100*r.height/data.height}%`});
  return <Theme theme={neutralTheme} mode="light"><div className="app">
    <header className="topbar"><div className="brand"><span className="mark">C</span><div><strong>Camel</strong><span>漫画阅读与校对</span></div></div>
      <div className="navigation"><Button label="上一页" size="sm" onClick={() => move(data.page-1)} isDisabled={!data || data.page===0 || loading}/>
        <label className="page-number"><span className="sr-only">页码</span><input type="number" min="1" max={data?.total ?? 1} value={number} onChange={e => setNumber(e.target.value)} onKeyDown={e => {if(e.key==='Enter') move(Number(number)-1)}}/></label>
        <span className="total">/ {data?.total ?? '—'}</span><Button label="跳转" size="sm" onClick={() => move(Number(number)-1)} isDisabled={!data || loading}/>
        <Button label="下一页" size="sm" onClick={() => move(data.page+1)} isDisabled={!data || data.page+1>=data.total || loading}/></div>
      <div className="layer-controls"><label><input type="checkbox" checked={showErase} onChange={e=>setShowErase(e.target.checked)}/>擦字</label><label><input type="checkbox" checked={showText} onChange={e=>setShowText(e.target.checked)}/>译文</label></div>
    </header>
    {error && <div role="alert" className="alert">{error}</div>}
    <main className="workspace"><section className="reader" aria-label="漫画页面"><div className="reader-meta"><span>{data?.name ?? '正在打开漫画'}</span><span>{loading ? '加载中…' : data ? `${data.width} × ${data.height}` : ''}</span></div>
      {data && <div className="page"><img className="page-original" src={data.original} alt={`漫画第 ${data.page+1} 页`}/>
        {showErase && data.erasures.map(p => <img key={`e${p.id}`} className="patch" style={position(p.bounds)} src={p.image} alt=""/>)}
        {showText && data.texts.map(p => <img key={`t${p.id}`} className="patch" style={position(p.bounds)} src={p.image} alt=""/>)}
        {data.dialogue.map(d => <button key={d.id} type="button" className={`hit ${selected===d.id?'active':''}`} style={position(d.bounds)} onClick={()=>select(d.id)} aria-label={`选择对白 ${d.id+1}`}/>)}
      </div>}
      {!data && <div className="empty">{loading ? '正在读取原始漫画…' : '无法打开页面'}</div>}
    </section><aside className="sidebar"><div className="sidebar-heading"><div><p className="eyebrow">当前页</p><h1>对白</h1></div><span className="count">{data?.dialogue.length ?? 0} 条</span></div>
      <p className="hint">点击画面中的气泡，或在这里修改译文。保存后会重新排版。</p>
      {dirty.length>0 && <div className="draft-bar"><span>{dirty.length} 处未保存</span><button onClick={()=>setDraft(Object.fromEntries(data.dialogue.map(d=>[d.id,d.translation??''])))}>放弃修改</button></div>}
      {data?.issues.length>0 && <details className="issues"><summary>{data.issues.length} 条待检查信息</summary><ul>{data.issues.map((s,i)=><li key={i}>{s}</li>)}</ul></details>}
      <div className="dialogues">{data?.dialogue.map((d,i)=><article id={`line-${d.id}`} key={d.id} className={`dialogue ${selected===d.id?'selected':''}`} onFocus={()=>setSelected(d.id)}><div className="line-title"><span>对白 {String(i+1).padStart(2,'0')}</span><span>#{d.id}</span></div><p className="source">{d.source || '未识别原文'}</p><label><span className="sr-only">对白 {i+1} 的译文</span><textarea rows="3" disabled={saving!==null} value={draft[d.id]??''} onChange={e=>setDraft({...draft,[d.id]:e.target.value})} placeholder="输入译文"/></label><div className="line-actions"><span>{(draft[d.id]??'') !== (d.translation??'') ? '已修改' : '已保存'}</span><Button label="保存" size="sm" variant="primary" isLoading={saving===d.id} isDisabled={saving!==null || (draft[d.id]??'')===(d.translation??'')} onClick={()=>save(d)}/></div></article>)}</div>
      {data && !data.dialogue.length && <div className="empty-side">这一页没有已识别的对白。</div>}
    </aside></main>
  </div></Theme>;
}
createRoot(document.getElementById('app')).render(<App/>);
