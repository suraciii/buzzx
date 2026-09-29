//! The browser surface: a loopback HTTP server over the existing session core.
//! The browser owns layout and input; Rust owns identity, relay access, and
//! action routing.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use crate::agents::Load as AgentLoad;
use crate::app::{
    AgentStatus, App, CommunityChoice, ConnState, Context, CreateChannelForm, CreateState, Filter,
    MemberRole, Mode, SearchScope, SearchTime,
};
use crate::client::ChannelKind;
use crate::config::{self, Resolved};
use crate::content::Row;
use crate::keys::Action;
use crate::session::SessionCommand;
const MAX_REQUEST: usize = 256 * 1024;
const SESSION_COOKIE: &str = "buzzx_session";
const HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>buzzx</title>
<style>
:root{color-scheme:light dark;--bg:#101318;--panel:#181d25;--panel2:#202733;--text:#edf1f7;--muted:#9aa6b5;--accent:#74b9ff;--danger:#ff7675;--line:#303947}
@media(prefers-color-scheme:light){:root{--bg:#f7f8fa;--panel:#fff;--panel2:#eef1f5;--text:#18212b;--muted:#5d6875;--accent:#1769aa;--danger:#b42318;--line:#d9dee7}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:16px/1.5 system-ui,sans-serif}button,input,textarea{font:inherit}button{border:1px solid var(--line);background:var(--panel2);color:var(--text);border-radius:6px;min-height:44px;padding:8px 12px;cursor:pointer}button:hover{border-color:var(--accent)}button:disabled{opacity:.5;cursor:not-allowed}input,textarea{width:100%;background:var(--panel);color:var(--text);border:1px solid var(--line);border-radius:6px;padding:10px}textarea{resize:vertical;min-height:80px}.app{display:grid;grid-template-columns:260px minmax(0,1fr);min-height:100vh}.sidebar{background:var(--panel);border-right:1px solid var(--line);padding:16px;display:flex;flex-direction:column;gap:12px}.brand{font-size:20px;font-weight:700}.identity{color:var(--muted);font-size:12px;overflow-wrap:anywhere}.filters{display:flex;gap:4px}.filters button{flex:1;padding:6px 4px;min-height:38px;font-size:13px}.filters button.active{border-color:var(--accent);color:var(--accent)}.channels{display:flex;flex-direction:column;gap:4px;overflow:auto}.channel{width:100%;text-align:left;border:0;background:transparent;display:flex;justify-content:space-between;gap:8px;padding:8px;min-height:44px}.channel.active{background:var(--panel2);color:var(--accent)}.count{color:var(--muted);font-size:13px}.sidebar-footer{margin-top:auto;color:var(--muted);font-size:12px}.main{min-width:0;display:flex;flex-direction:column}.topbar{display:flex;align-items:center;gap:8px;flex-wrap:wrap;padding:12px 20px;border-bottom:1px solid var(--line);background:var(--panel)}.title{font-weight:700;margin-right:auto}.status{color:var(--muted);font-size:13px}.content{width:min(880px,100%);margin:0 auto;padding:18px 20px;flex:1;display:flex;flex-direction:column;min-height:0}.toolbar{display:flex;gap:8px;flex-wrap:wrap;margin-bottom:12px}.rows{display:flex;flex-direction:column;gap:12px;overflow:auto;min-height:120px}.row{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:12px}.row.focus{border-color:var(--accent)}.meta{display:flex;gap:8px;align-items:baseline;color:var(--muted);font-size:13px}.author{font-weight:650;color:var(--text)}.body{white-space:pre-wrap;overflow-wrap:anywhere;margin:6px 0}.body pre{white-space:pre-wrap;overflow:auto}.row-actions{display:flex;gap:6px;flex-wrap:wrap}.row-actions button{min-height:36px;font-size:13px;padding:5px 9px}.composer{border-top:1px solid var(--line);background:var(--panel);padding:12px 20px}.composer-label{font-size:13px;color:var(--muted);margin-bottom:6px}.composer-controls{display:flex;gap:8px;align-items:flex-end}.composer-controls textarea{flex:1}.notice{padding:8px 10px;background:var(--panel2);border-radius:6px;margin-bottom:10px}.error{color:var(--danger)}.empty{color:var(--muted);padding:28px 0;text-align:center}.search-result{cursor:pointer}.search{display:flex;flex-direction:column;gap:12px}.search-form{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:8px}.search-result{cursor:pointer}.back{margin-right:4px}.agent{padding:10px;background:var(--panel);border:1px solid var(--line);border-radius:6px;margin-bottom:8px}.small{font-size:13px;color:var(--muted)}
@media(max-width:899px){.app{display:block}.sidebar{min-height:auto;border-right:0;border-bottom:1px solid var(--line)}.channels{max-height:220px}.content{padding:14px 12px}.composer{padding:10px 12px}.topbar{padding:10px 12px}.desktop-only{display:none}}
</style>
</head>
<body><div id="root"></div>
<script>
const root=document.getElementById('root');root.textContent='Connecting…';window.addEventListener('error',event=>{root.textContent='Browser error: '+event.message});const tabKey=globalThis.crypto?.randomUUID?.()||Math.random().toString(36).slice(2);let state=null;let view='channel';let lastView='';let notice='';let drafts=new Map();let fields=new Map();let refreshSerial=0;let mentionQueue=Promise.resolve();
const esc=(x)=>String(x??'');function draftFor(){const key=(state?.community?.id||'default')+':'+(state?.selected||'inbox');if(!drafts.has(key))drafts.set(key,{text:'',target:null,pending:false,caret:null});return drafts.get(key)}function saveDraft(){const box=document.querySelector('#composer');if(box&&!box.disabled)draftFor().text=box.value}
async function api(path,body){const r=await fetch(path,{method:body?'POST':'GET',headers:body?{'content-type':'application/json','x-buzzx-tab':tabKey}:{'x-buzzx-tab':tabKey},body:body?JSON.stringify(body):undefined,credentials:'same-origin'});if(!r.ok){throw new Error(await r.text()||r.statusText)}return r.json()}
function rowHtml(r,selected){return `<article class="row ${selected?'focus':''}" data-row-id="${esc(r.event_id)}"><div class="meta"><span class="author"></span><span>${new Date((r.created_at||0)*1000).toLocaleString()}</span>${r.edited?'<span>(edited)</span>':''}${r.uncertain?'<span class="error">Unknown result</span>':''}</div><div class="body"></div><div class="row-actions"><button data-act="reply" data-id="${esc(r.event_id)}">Reply</button><button data-act="thread" data-id="${esc(r.event_id)}">Open thread</button><button data-act="reader" data-id="${esc(r.event_id)}">Read message</button><button data-act="react" data-id="${esc(r.event_id)}">Like</button>${r.own?' <button data-act="edit" data-id="'+esc(r.event_id)+'">Edit</button><button data-act="delete" data-id="'+esc(r.event_id)+'">Delete</button>':''}</div></article>`}
function captureFocus(){const el=document.activeElement;if(el?.hasAttribute?.('data-lifecycle'))lifecycleHeaderFocus=el.textContent;if(!el||el===document.body||!el.id||!root.contains(el))return null;const caret=el.tagName==='INPUT'||el.tagName==='TEXTAREA'?{start:el.selectionStart,end:el.selectionEnd}:null;return{id:el.id,caret}}
function restoreFocus(seed){if(!seed)return;const el=root.querySelector('#'+CSS.escape(seed.id));if(!el)return;el.focus({preventScroll:true});if(seed.caret&&el.setSelectionRange)try{el.setSelectionRange(seed.caret.start,seed.caret.end)}catch(_){}}
function render(){if(!state){root.textContent='Connecting…';return}const seed=captureFocus();const scrollY=window.scrollY;const anchorEl=root.querySelector('.rows .row.focus');const anchor=anchorEl&&anchorEl.dataset.rowId?{id:anchorEl.dataset.rowId,top:anchorEl.getBoundingClientRect().top}:null;const rowSeed=anchor?anchor.id:null;root.innerHTML='';const app=document.createElement('div');app.className='app';app.innerHTML=`<aside class="sidebar"><div class="brand">buzzx</div><div class="identity"></div><div class="community"></div><input id="find" placeholder="Find conversation" aria-label="Find conversation"><div class="filters"><button data-filter="All">All</button><button data-filter="Unread">Unread</button><button data-filter="For you">For you</button></div><div class="channels"></div><div class="sidebar-footer"></div></aside><main class="main"><header class="topbar"><button class="back" data-act="back">Inbox</button><div class="title"></div><span class="status"></span><button data-act="search">Search messages</button><button data-act="agents">My agents</button><button data-act="help">Help</button></header><section class="content"></section><footer class="composer"><div class="composer-label"></div><div class="composer-controls"><textarea id="composer" placeholder="Write a message…"></textarea><button data-act="send">Send</button></div></footer></main></div>`;root.appendChild(app);app.querySelector('.identity').textContent=state.identity+' · '+state.relay;const community=app.querySelector('.community');community.textContent='Community: '+(state.community?.name||'default')+' · '+(state.connection||'Connecting');(state.communities||[]).forEach(c=>{const b=document.createElement('button');b.textContent=c.name+(c.active?' ✓':'');b.title=c.relay_url;b.onclick=()=>act({action:'community',community:c.id});community.appendChild(b)});app.querySelector('.status').textContent=state.connection+(state.startup_error?' · '+state.startup_error:'');app.querySelector('.title').textContent=state.title||'Inbox';app.querySelector('.sidebar-footer').textContent=state.status||'';const list=app.querySelector('.channels');const empty=document.createElement('div');empty.className='empty';list.appendChild(empty);state.channels.forEach(c=>{const b=document.createElement('button');b.className='channel'+(c.id===state.selected?' active':'');b.dataset.channel=c.id;b.dataset.matched=c.matched?'1':'';b.hidden=!c.matched;const n=document.createElement('span');n.textContent=c.name;b.appendChild(n);if(c.unread||c.marker==='unknown'){const x=document.createElement('span');x.className='count';x.textContent=c.marker==='unknown'?'?':String(c.unread);b.appendChild(x)}list.appendChild(b)});app.querySelectorAll('[data-filter]').forEach(b=>{const on=b.dataset.filter===state.filter;b.classList.toggle('active',on);b.setAttribute('aria-pressed',on?'true':'false')});const cinput=app.querySelector('#find');cinput.value=fields.get('find')||'';const applyFind=()=>{const q=cinput.value.toLowerCase();let shown=0;list.querySelectorAll('.channel').forEach(b=>{b.hidden=b.dataset.matched!=='1'||!b.textContent.toLowerCase().includes(q);if(!b.hidden)shown++});empty.textContent=shown?'':(q?'No match':(state.filter_empty||''))};cinput.addEventListener('input',()=>{fields.set('find',cinput.value);applyFind()});applyFind();app.querySelector('.composer-label').textContent=state.composer_label||'New message';const content=app.querySelector('.content');
if(view==='search'){renderSearch(content)}else if(view==='agents'){renderAgents(content)}else if(view==='help'){content.innerHTML='<h2>Help</h2><p>Enter sends. Shift+Enter inserts a newline. Search and inspection keep drafts in this tab. Browser Back returns through the current inspection path.</p><p>Only the local process can authorize this page. Refreshing or closing the tab does not persist drafts.</p>'}else if(view==='reader'){renderReader(content)}else if(view==='thread'){renderRows(content,state.thread,'Thread')}else if(view==='context'){renderRows(content,state.context,'Search context')}else{renderRows(content,state.rows,state.title||'Conversation')}
wire(app);const d=draftFor();const ta=app.querySelector('#composer');ta.value=d.text;ta.disabled=!!d.pending;ta.oninput=()=>draftFor().text=ta.value;app.querySelector('[data-act="send"]').disabled=!!d.pending;app.querySelectorAll('[data-channel]').forEach(b=>b.onclick=()=>selectChannel(b.dataset.channel));app.querySelector('[data-act="agents"]').onclick=()=>act({action:'agents'});app.querySelector('[data-act="help"]').onclick=()=>act({action:'help'});restoreFocus(seed);if(d.caret!=null){ta.focus({preventScroll:true});try{ta.setSelectionRange(d.caret,d.caret)}catch(_){}d.caret=null}const rowNow=root.querySelector('.rows .row.focus');const rowId=rowNow?rowNow.dataset.rowId:null;if(view!==lastView){lastView=view;window.scrollTo(0,0)}else if(rowNow&&rowId!==rowSeed)rowNow.scrollIntoView({block:'nearest'});else if(anchor){const was=root.querySelector('[data-row-id="'+CSS.escape(anchor.id)+'"]');if(was)window.scrollBy(0,was.getBoundingClientRect().top-anchor.top);else window.scrollTo(0,scrollY)}else window.scrollTo(0,scrollY);}
function renderRows(content,rows,title){content.innerHTML='<div class="toolbar"><h2></h2><button data-act="older">Older</button><button data-act="newer">Newer</button><button data-act="latest">Latest</button></div>';content.querySelector('h2').textContent=title;const box=document.createElement('div');box.className='rows';content.appendChild(box);const isThread=title==='Thread';const loading=isThread?state.thread_loading:state.context_loading;const failed=isThread?state.thread_failed:state.context_failed;if(loading)box.innerHTML='<div class="loading">Loading…</div>';if(failed){const error=document.createElement('div');error.className='error';error.textContent='Read failed: '+failed+'; use Back and retry.';box.appendChild(error)}rows.forEach((r,i)=>{const holder=document.createElement('div');holder.innerHTML=rowHtml(r,i===(isThread?state.thread_focus:state.context_focus));const article=holder.firstElementChild;article.querySelector('.author').textContent=r.author;article.querySelector('.body').textContent=r.body;box.appendChild(article)});if(!rows.length&&!loading&&!failed)box.insertAdjacentHTML('beforeend','<div class="empty">No messages loaded.</div>')}
function renderSearch(content){content.innerHTML='<div class="search"><div class="toolbar"><button data-act="back">Back</button></div><div class="search-form"><input id="query" placeholder="Search messages"><button id="submit-search">Search</button></div><div class="toolbar"><label>Scope <select id="scope"><option value="current">Current conversation</option><option value="all">All listed conversations</option></select></label><label>Author <input id="author" placeholder="Exact public key (optional)"></label><label>Time <select id="time"><option value="all">All time</option><option value="7d">Last 7 days</option><option value="30d">Last 30 days</option></select></label></div><div class="search-status"></div><div class="rows"></div></div>';const query=content.querySelector('#query');query.value=fields.get('query')??(state.search_query||'');query.addEventListener('input',()=>fields.set('query',query.value));const author=content.querySelector('#author');author.value=fields.get('author')||'';author.addEventListener('input',()=>fields.set('author',author.value));const submitSearch=()=>act({action:'search',query:query.value,scope:content.querySelector('#scope').value,author:author.value,time:content.querySelector('#time').value});query.addEventListener('keydown',e=>{if(e.key==='Enter'&&!e.isComposing){e.preventDefault();submitSearch()}});author.addEventListener('keydown',e=>{if(e.key==='Enter'&&!e.isComposing){e.preventDefault();submitSearch()}});content.querySelector('#submit-search').onclick=submitSearch;const box=content.querySelector('.rows');(state.search||[]).forEach((r,i)=>{const holder=document.createElement('div');holder.innerHTML=rowHtml(r,i===state.search_focus);const article=holder.firstElementChild;article.classList.add('search-result');article.title='Open in context';article.querySelector('.author').textContent=r.author;article.querySelector('.body').textContent=r.body;article.addEventListener('click',event=>{if(event.target.closest('button'))return;act({action:'context',event:r.event_id})});box.appendChild(article)});const hidden=state.search_hidden>0?` · bounded: ${state.search_hidden} hidden by visibility`:'';const applied=!!state.search_applied_query||!!state.search_applied_author;let status='';if(state.search_failed)status=`search failed: ${state.search_failed}`+(state.search_previous?` · results from previous query ${state.search_previous}`:'');else if(state.search_loading)status='searching…'+(state.search_previous?` · showing previous query ${state.search_previous}`:'');else if(state.search_bounded)status=`Top 50; narrow filters${hidden}`;else if(state.search.length)status=`${state.search.length} results returned${hidden}`;else if(applied)status=`no returned matches${hidden}`;content.querySelector('.search-status').textContent=status;if(!state.search||!state.search.length)box.innerHTML=applied?`<div class="empty">no returned matches${hidden}</div>`:'<div class="empty">Enter a query and select Search.</div>';content.querySelector('#scope').value=state.search_scope||'current';content.querySelector('#author').value=state.search_author||'';content.querySelector('#time').value=state.search_time||'all';const scope=content.querySelector('#scope');state.channels.forEach(c=>{const option=document.createElement('option');option.value=c.id;option.textContent='Conversation: '+c.name;scope.appendChild(option)});scope.value=state.search_scope||'current';if(state.search_loading)content.insertAdjacentHTML('afterbegin','<div class="loading">Searching…</div>');if(state.search_failed)content.insertAdjacentHTML('afterbegin','<div class="error">Search failed: '+esc(state.search_failed)+'; previous results are retained.</div>');if(state.search_bounded)content.insertAdjacentHTML('afterbegin','<div class="small">Showing the relay result bound; more matches may exist.</div>')}
function renderReader(content){const r=state.reader;content.innerHTML='<div class="toolbar"><button data-act="back">Back</button></div><h2>Read message</h2><div class="row"><div class="meta"><span class="author"></span></div><div class="body"></div><div class="row-actions"><button data-act="reply" data-id=""></button></div></div>';content.querySelector('.author').textContent=r?r.author:'';content.querySelector('.body').textContent=r?r.body:'Message unavailable';content.querySelector('[data-act="reply"]').textContent='Reply';if(r)content.querySelector('[data-act="reply"]').dataset.id=r.event_id}
function renderAgents(content){content.innerHTML='<div class="toolbar"><button data-act="back">Back</button></div><h2>My agents</h2>';if(!state.agents||!state.agents.length){content.innerHTML+='<div class="empty">No owned Agents loaded.</div>';return}state.agents.forEach(a=>{const d=document.createElement('div');d.className='agent';const name=document.createElement('strong');name.textContent=a.name;d.appendChild(name);const status=document.createElement('div');status.className='small';status.textContent=a.status;d.appendChild(status);const detail=document.createElement('div');detail.className='small';detail.textContent=(a.contexts||[]).map(c=>c.kind==='channel'?c.name:c.kind==='unavailable'?'Unavailable conversation':'Scheduled turn').join(', ')||'No observed working context';d.appendChild(detail);content.appendChild(d)})}
function rowViewport(action,page){if(view!=='channel')return act({action});if(page<0)return window.scrollBy({top:-Math.round(window.innerHeight*0.9),behavior:'smooth'});if(page>0)return window.scrollBy({top:Math.round(window.innerHeight*0.9),behavior:'smooth'});window.scrollTo({top:document.documentElement.scrollHeight})}
function wire(app){app.querySelectorAll('[data-filter]').forEach(b=>b.onclick=()=>act({action:'filter',filter:b.dataset.filter}));app.querySelectorAll('[data-act]').forEach(b=>b.onclick=()=>{const a=b.dataset.act,id=b.dataset.id;if(a==='reply')return reply(id);if(a==='edit')return edit(id);if(a==='delete')return act({action:'delete',event:id});if(a==='react')return act({action:'react',event:id});if(a==='thread')return act({action:'thread',event:id});if(a==='reader')return act({action:'reader',event:id});if(a==='send')return send();if(a==='new')return act({action:'new'});if(a==='search')return setView('search');if(a==='submit-search')return act({action:'search',query:document.querySelector('#query').value});if(a==='agents')return setView('agents');if(a==='help')return setView('help');if(a==='back')return back();if(a==='older')return rowViewport('older',-1);if(a==='newer')return rowViewport('newer',1);if(a==='latest')return rowViewport('latest',0)});const ta=app.querySelector('#composer');ta.onkeydown=e=>{if(e.isComposing)return;if(state?.mention_suggestions&&!e.shiftKey&&!e.ctrlKey&&!e.altKey&&!e.metaKey){const action={'ArrowDown':()=>mentionAction({action:'mention_move',delta:1}),'ArrowUp':()=>mentionAction({action:'mention_move',delta:-1}),'Enter':()=>mentionAction({action:'mention_confirm'}),'Escape':()=>mentionAction({action:'mention_close'})}[e.key];if(action){e.preventDefault();action();return}}if(e.key==='Enter'&&!e.shiftKey){e.preventDefault();send()}}}
async function selectChannel(id){saveDraft();const d=draftFor();if(d.text.trim())try{await mentionQueue;await api('/api/action',{action:'draft',content:d.text})}catch(e){notice=e.message;return}await act({action:'select',channel:id})}
function reconcileDraft(){const d=draftFor();if(!d.pending)return;if(state.write_pending)return;if(state.composer_mode&&state.composer_text){d.text=state.composer_text;d.pending=false;return}d.text='';d.target=null;d.pending=false}
async function mentionAction(body){mentionQueue=mentionQueue.then(async()=>{try{saveDraft();const result=await api('/api/action',body);const d=draftFor();if(typeof result.composer_text==='string'){d.text=result.composer_text;d.caret=utf16Offset(result.composer_text,result.composer_cursor||0)}await refresh()}catch(e){notice=e.message;await refresh()}});return mentionQueue}
async function act(body){try{saveDraft();const result=await api('/api/action',body);if(result.view){const previous=view;view=result.view;if(view!==previous&&body.action!=='back')history.pushState({view},'', '#'+view);else history.replaceState({view},'', '#'+view)}await refresh()}catch(e){notice=e.message;await refresh()}}
async function send(){saveDraft();const d=draftFor();if(!d.text.trim()||d.pending)return;d.pending=true;try{await mentionQueue;await api('/api/action',{action:'send',content:d.text,event:d.target})}catch(e){d.pending=false;notice=e.message}await refresh()}
function reply(id){saveDraft();draftFor().target=id;act({action:'reply',event:id})}function edit(id){saveDraft();draftFor().target=id;act({action:'edit',event:id})}function setView(v){saveDraft();view=v;history.pushState({view},'', '#'+view);refresh()}function back(){act({action:'back'})}
async function refresh(){const serial=++refreshSerial;try{const next=await api('/api/state');if(serial!==refreshSerial)return;if(state&&typeof next.generation==='number'&&typeof state.generation==='number'&&next.generation<state.generation)return;state=next;reconcileDraft();if(!view||view==='channel')view=state.surface||'channel';if(notice){state.status=notice;notice=''}render();reportPresented()}catch(e){if(serial===refreshSerial)root.textContent='Session unavailable: '+e.message}}
async function browserBack(){if(window.backInFlight)return;window.backInFlight=true;try{await act({action:'back'})}finally{window.backInFlight=false}}document.addEventListener('visibilitychange',reportPresented);window.addEventListener('focus',reportPresented);window.addEventListener('popstate',browserBack);window.addEventListener('hashchange',browserBack);window.addEventListener('load',refresh,{once:true});setInterval(refresh,1200);
async function reportPresented(){if(!state||view!=='channel'||document.visibilityState!=='visible'||!document.hasFocus()||!state.rows.length||state.focus!==state.rows.length-1)return;const box=document.querySelector('.rows'),last=state.rows[state.rows.length-1];if(!box||window.scrollY+window.innerHeight<document.documentElement.scrollHeight-2)return;try{await api('/api/action',{action:'presented',visible:true,latest:last.event_id})}catch(_){}}
const utf8Offset=(text,units)=>new TextEncoder().encode(text.slice(0,units)).length;
const utf16Offset=(text,bytes)=>{let units=0,seen=0;for(const ch of text){if(seen>=bytes)break;seen+=new TextEncoder().encode(ch).length;units+=ch.length}return units};
function mentionInput(ta){const cursor=utf8Offset(ta.value,ta.selectionStart||0);mentionAction({action:'mention',content:ta.value,cursor});}
function mentionAvatar(candidate){const avatar=document.createElement('span');avatar.className='mention-avatar';if(candidate.picture){const image=document.createElement('img');image.src=candidate.picture;image.alt='';image.referrerPolicy='no-referrer';image.onerror=()=>{image.remove();avatar.textContent=(candidate.label||'?').trim().slice(0,2).toUpperCase()};avatar.appendChild(image)}else avatar.textContent=(candidate.label||'?').trim().slice(0,2).toUpperCase();return avatar}
function renderWebExtras(){const ta=document.querySelector('#composer');if(!ta)return;let pop=document.querySelector('.mention-popover');const suggestions=state?.mention_suggestions;if(suggestions){if(!pop){pop=document.createElement('div');pop.className='mention-popover';ta.parentElement.parentElement.appendChild(pop)}pop.replaceChildren();if(suggestions.loading){pop.textContent='Loading members…'}else if(suggestions.failed){const text=document.createElement('span');text.textContent='Member list failed: '+suggestions.failed;pop.append(text);const retry=document.createElement('button');retry.textContent='Retry';retry.onclick=()=>act({action:'mention_retry'});pop.append(retry)}else if(!suggestions.items.length){pop.textContent='No matching members'}else{const counts={};suggestions.items.forEach(item=>counts[item.label]=(counts[item.label]||0)+1);suggestions.items.forEach((item,index)=>{const row=document.createElement('button');row.className='mention-row'+(index===suggestions.selected_index?' selected':'');row.append(mentionAvatar(item));const label=document.createElement('span');label.textContent=item.label;row.append(label);if(item.agent){const marker=document.createElement('b');marker.textContent=' Agent';row.append(marker)}if(item.admin){const marker=document.createElement('b');marker.textContent=' Admin';row.append(marker)}if(counts[item.label]>1){const key=document.createElement('small');key.textContent=' '+item.short_key;row.append(key)}row.onclick=()=>mentionAction({action:'mention_select',index}).then(()=>mentionAction({action:'mention_confirm'}));pop.append(row)})}}else if(pop)pop.remove()}
let createDraft=null;
function buildCreateModal(form,communityId){
  let modal=document.querySelector('.create-channel-modal');
  if(!modal){modal=document.createElement('div');modal.className='create-channel-modal';document.body.append(modal)}
  modal.replaceChildren();
  const backdrop=document.createElement('div');backdrop.className='create-channel-backdrop';backdrop.onclick=()=>act({action:'create_channel_close'});modal.append(backdrop);
  const panel=document.createElement('div');panel.className='create-channel-panel';panel.setAttribute('role','dialog');panel.setAttribute('aria-modal','true');panel.setAttribute('aria-label','Create channel');modal.append(panel);
  const heading=document.createElement('strong');heading.textContent='Create channel · '+(state?.community?.name||'current community');panel.append(heading);
  const locked=form.state==='creating'||form.state==='unknown';
  const addField=(label,node)=>{const wrap=document.createElement('label');wrap.textContent=label;wrap.append(node);panel.append(wrap);return node};
  const name=document.createElement('input');name.value=createDraft.name;name.placeholder='Name';name.disabled=locked;name.oninput=()=>{createDraft.name=name.value;syncSubmit()};addField('Name',name);
  const kind=document.createElement('select');[['stream','stream'],['forum','forum']].forEach(([value,text])=>{const option=document.createElement('option');option.value=value;option.textContent=text;if(createDraft.type===value)option.selected=true;kind.append(option)});kind.disabled=locked;kind.onchange=()=>{createDraft.type=kind.value};addField('Type',kind);
  const visibility=document.createElement('select');[['open','open'],['private','private']].forEach(([value,text])=>{const option=document.createElement('option');option.value=value;option.textContent=text;if(createDraft.visibility===value)option.selected=true;visibility.append(option)});visibility.disabled=locked;visibility.onchange=()=>{createDraft.visibility=visibility.value};addField('Visibility',visibility);
  const description=document.createElement('textarea');description.value=createDraft.description;description.placeholder='Description (optional)';description.disabled=locked;description.oninput=()=>{createDraft.description=description.value};addField('Description',description);
  if(form.failure){const error=document.createElement('div');error.className='error';error.textContent=form.state==='unknown'?'The channel may already exist. Refresh the list to confirm before creating another. '+form.failure:form.failure;panel.append(error)}
  const actions=document.createElement('div');actions.className='create-channel-actions';panel.append(actions);
  const submit=document.createElement('button');submit.textContent=form.state==='creating'?'Creating…':'Create';submit.dataset.createSubmit='';submit.onclick=()=>{submit.disabled=true;act({action:'create_channel_submit',community:communityId,name:createDraft.name,type:createDraft.type,visibility:createDraft.visibility,description:createDraft.description})};actions.append(submit);
  // The name decides whether this submission can go: the button follows every
  // keystroke, not only the panel's first build.
  const syncSubmit=()=>{submit.disabled=locked||!createDraft.name.trim()};
  syncSubmit();
  if(form.state==='unknown'){const retry=document.createElement('button');retry.textContent='Refresh list';retry.dataset.createRefresh='';retry.onclick=()=>act({action:'create_channel_refresh'});actions.append(retry)}
  const cancel=document.createElement('button');cancel.textContent='Cancel';cancel.onclick=()=>act({action:'create_channel_close'});actions.append(cancel);
  const hint=document.createElement('div');hint.className='small';hint.textContent='Name is required. The current community in the bar is where this channel is created.';panel.append(hint);
  name.focus();
}
function renderCreateExtras(){
  const sidebar=document.querySelector('.sidebar');
  if(sidebar&&!sidebar.querySelector('[data-create-channel]')){const button=document.createElement('button');button.dataset.createChannel='';button.textContent='+ Create channel';button.onclick=()=>act({action:'create_channel'});sidebar.insertBefore(button,sidebar.querySelector('.channels'))}
  const form=state?.create_channel;
  // The write outlives the modal: while it is open the page keeps a surface
  // that says so and offers the refresh which settles it.
  const unsettled=state?.create_unsettled;
  let block=document.querySelector('.create-unsettled');
  if(unsettled&&!form){
    if(!block){block=document.createElement('div');block.className='create-unsettled notice';document.body.append(block)}
    block.replaceChildren();
    const title=document.createElement('strong');title.textContent=(unsettled.state==='unknown'?'Channel creation unresolved: ':'Creating channel: ')+unsettled.name;block.append(title);
    const detail=document.createElement('div');detail.textContent=unsettled.state==='unknown'?'The relay did not confirm it. Refresh the list to see whether it exists; creating it again could make a second channel.':'The relay has not answered yet.';block.append(detail);
    const refresh=document.createElement('button');refresh.textContent='Refresh list';refresh.dataset.createRefresh='';refresh.onclick=()=>act({action:'create_channel_refresh'});block.append(refresh);
    const review=document.createElement('button');review.textContent='Open the form';review.onclick=()=>act({action:'create_channel'});block.append(review);
  } else if(block) block.remove();
  if(!form){document.querySelector('.create-channel-modal')?.remove();createDraft=null;return}
  const communityId=state?.community?.id||'';
  // The panel is rebuilt only when the session's form state or community
  // changes: a rebuild between keystrokes would take the caret with it.
  if(createDraft&&createDraft.community===communityId&&createDraft.state===form.state&&document.querySelector('.create-channel-modal'))return;
  createDraft={community:communityId,state:form.state,name:form.name,type:form.type,visibility:form.visibility,description:form.description};
  buildCreateModal(form,communityId);
}
document.addEventListener('keydown',event=>{if(event.key==='Escape'&&document.querySelector('.create-channel-modal')){event.preventDefault();act({action:'create_channel_close'})}});
document.addEventListener('input',event=>{if(event.target?.id==='composer')mentionInput(event.target)});setInterval(()=>{renderWebExtras();renderCreateExtras()},100);
function renderMentionBlock(){const composer=document.querySelector('.composer');if(!composer)return;let block=composer.querySelector('.mention-block');if(!state?.mention_block){if(block)block.remove();return}if(!block){block=document.createElement('div');block.className='mention-block notice';composer.prepend(block)}block.replaceChildren();const title=document.createElement('strong');title.textContent=state.mention_block.kind+': '+state.mention_block.summary;block.append(title);const detail=document.createElement('div');detail.textContent=(state.mention_block.details||[]).join(' ');block.append(detail);const retry=document.createElement('button');retry.textContent='Edit and retry';retry.onclick=()=>document.querySelector('#composer')?.focus();block.append(retry)}
setInterval(renderMentionBlock,100);
const webExtraStyle=document.createElement('style');webExtraStyle.textContent='.mention-popover{position:relative;z-index:5;background:var(--panel);border:1px solid var(--line);padding:6px;max-height:280px;overflow:auto}.mention-row{display:flex;align-items:center;gap:8px;width:100%;text-align:left}.mention-row.selected{border-color:var(--accent)}.mention-avatar{width:28px;height:28px;border-radius:50%;background:var(--panel2);display:inline-flex;align-items:center;justify-content:center;font-size:12px}.mention-avatar img{width:100%;height:100%;border-radius:50%}.create-channel-modal{position:fixed;inset:0;z-index:40;display:flex;align-items:center;justify-content:center}.create-channel-backdrop{position:absolute;inset:0;background:rgba(0,0,0,.55)}.create-channel-panel{position:relative;z-index:1;background:var(--panel);border:1px solid var(--line);padding:14px;display:flex;flex-direction:column;gap:8px;min-width:320px;max-width:min(560px,92vw);max-height:88vh;overflow:auto}.create-channel-panel label{display:flex;flex-direction:column;gap:4px;font-size:12px}.create-channel-actions{display:flex;gap:6px;justify-content:flex-end}.create-channel-panel input,.create-channel-panel select,.create-channel-panel textarea{min-height:32px}.create-unsettled{position:fixed;left:50%;transform:translateX(-50%);bottom:16px;z-index:30;background:var(--panel);border:1px solid var(--line);padding:10px 12px;display:flex;flex-direction:column;gap:6px;max-width:min(560px,92vw)}';document.head.append(webExtraStyle);
// Lifecycle controls are deliberately state-driven: a tab never invents a
// successful archive, role, or membership row while readback is pending.
let lifecycleFingerprint='',lifecycleHeaderFingerprint='',lifecycleHeaderFocus=null,archiveFingerprint='',memberQueryTimer=null;
const lifecycleRole=(d)=>['owner','admin'].includes(d?.my_role);
const lcButton=(parent,text,action,disabled=false)=>{const b=document.createElement('button');b.textContent=text;b.disabled=disabled;b.onclick=()=>act(action);parent.append(b);return b};
function lcModal(className,title,body){document.querySelector('.'+className)?.remove();const m=document.createElement('div');m.className=className+' lifecycle-modal';const p=document.createElement('div');p.className='lifecycle-panel';p.setAttribute('role','dialog');p.setAttribute('aria-modal','true');p.setAttribute('aria-label',title);const h=document.createElement('h2');h.textContent=title;p.append(h);p.append(body);m.append(p);document.body.append(m);return p}
function lifecycleModalClose(){document.querySelectorAll('.lifecycle-modal').forEach(x=>x.remove())}
function lifecycleAdminAction(){const d=state?.channel_details;if(!d)return;const p=lcModal('channel-sheet','Channel actions',document.createElement('div'));const detail=document.createElement('p');detail.textContent=`${d.name} · ${d.kind} · ${d.visibility||'unknown'} · ${d.archived?'Archived':'Active'} · role: ${d.my_role||'unknown'}`;p.append(detail);if(d.description){const about=document.createElement('p');about.textContent=d.description;p.append(about)}const open=(text,action)=>{const b=lcButton(p,text,action);b.onclick=()=>{p.parentElement.remove();act(action)}};open('Manage members',{action:'members',channel:state.selected});if(lifecycleRole(d)){open('Edit channel',{action:'edit_channel'});open(d.archived?'Unarchive':'Archive',{action:d.archived?'unarchive':'archive',channel:state.selected})}const close=document.createElement('button');close.textContent='Close';close.onclick=()=>p.parentElement.remove();p.append(close);p.querySelector('button')?.focus()}
function renderLifecycleHeader(){const bar=document.querySelector('.topbar'),d=state?.channel_details;if(!bar)return;if(!d){bar.querySelectorAll('[data-lifecycle]').forEach(x=>x.remove());lifecycleHeaderFingerprint='';lifecycleHeaderFocus=null;return}const fp=JSON.stringify([state.selected,d]);if(fp===lifecycleHeaderFingerprint&&bar.querySelector('[data-lifecycle]'))return;lifecycleHeaderFingerprint=fp;bar.querySelectorAll('[data-lifecycle]').forEach(x=>x.remove());const mark=(text,fn,wide=true)=>{const b=document.createElement('button');b.dataset.lifecycle='';b.textContent=text;b.className=wide?'desktop-only':'';b.onclick=fn;bar.insertBefore(b,bar.querySelector('[data-act="search"]'));};mark('Channel details',lifecycleAdminAction,false);mark('Members',()=>act({action:'members',channel:state.selected}),true);if(lifecycleRole(d)){mark('Edit channel',()=>act({action:'edit_channel'}),true);mark(d.archived?'Unarchive':'Archive',()=>act({action:d.archived?'unarchive':'archive',channel:state.selected}),true)}if(lifecycleHeaderFocus){const button=[...bar.querySelectorAll('[data-lifecycle]')].find(x=>x.textContent===lifecycleHeaderFocus);button?.focus({preventScroll:true});lifecycleHeaderFocus=null}}
function lifecycleField(p,label,value,tag='input'){const l=document.createElement('label');l.textContent=label;const e=document.createElement(tag);e.value=value||'';l.append(e);p.append(l);return e}
function renderEditLifecycle(){const f=state?.edit_channel;if(!f){document.querySelector('.edit-channel-modal')?.remove();return}if(document.querySelector('.edit-channel-modal')?.dataset.state===f.state)return;const p=lcModal('edit-channel-modal','Edit channel',document.createElement('div'));p.parentElement.dataset.state=f.state;const n=lifecycleField(p,'Name',f.name),d=lifecycleField(p,'Description',f.description,'textarea'),v=lifecycleField(p,'Visibility',f.visibility,'select');['open','private'].forEach(x=>{const o=document.createElement('option');o.value=x;o.textContent=x;o.selected=x===f.visibility;v.append(o)});if(f.failure){const e=document.createElement('p');e.className='error';e.textContent=f.failure;p.append(e)}const submit=lcButton(p,f.state==='creating'?'Saving…':'Save',{action:'edit_channel_submit',community:state.community?.id,name:n.value,description:d.value,visibility:v.value},f.state==='creating'||f.state==='unknown');submit.onclick=()=>act({action:'edit_channel_submit',community:state.community?.id,name:n.value,description:d.value,visibility:v.value});if(f.state==='unknown')lcButton(p,'Refresh', {action:'edit_channel_refresh'});lcButton(p,'Cancel',{action:'edit_channel_close'});n.focus()}
function memberRow(p,m,admin){const row=document.createElement('div');row.className='member-row';const key=m.pubkey.slice(0,8)+'…';const text=document.createElement('span');text.textContent=`${m.label||key} · ${m.role||'unknown'} · ${key}`;row.append(text);if(admin&&!m.is_self&&!m.last_owner){const role=document.createElement('select');['owner','admin','member','guest','bot'].forEach(r=>{const o=document.createElement('option');o.value=r;o.textContent=r;o.selected=r===m.role;role.append(o)});const rb=lcButton(row,'Change role',{action:'members_role_open',pubkey:m.pubkey,role:role.value});rb.onclick=()=>act({action:'members_role_open',pubkey:m.pubkey,role:role.value});row.append(role);const rm=lcButton(row,'Remove',{action:'members_remove_open',pubkey:m.pubkey});rm.className='danger'}p.append(row)}
function renderMembersLifecycle(){const m=state?.members;if(!m){document.querySelector('.members-modal')?.remove();lifecycleFingerprint='';return}if(m.community_id&&m.community_id!==state.community?.id||m.generation!=null&&m.generation!==state.generation)return;const fp=JSON.stringify(m);if(fp===lifecycleFingerprint)return;const oldQuery=document.querySelector('.members-modal [data-member-query]'),queryFocused=oldQuery&&document.activeElement===oldQuery,queryCaret=queryFocused?oldQuery.selectionStart:null;const previousExact=document.querySelector('.members-modal [data-exact-key]')?.value||'';lifecycleFingerprint=fp;const p=lcModal('members-modal','Manage members',document.createElement('div'));const admin=lifecycleRole({my_role:m.my_role});let q=null;if(m.loading){const x=document.createElement('p');x.textContent='Loading members…';p.append(x)}if(m.failed){const x=document.createElement('p');x.className='error';x.textContent='Member list failed: '+m.failed;p.append(x);lcButton(p,'Retry',{action:'members_refresh'})}(m.members||[]).forEach(x=>memberRow(p,x,admin));if(admin){lcButton(p,'Add members',{action:'members_add_open'});if(m.picker?.open){q=lifecycleField(p,'Search identities',m.picker.query);q.dataset.memberQuery='';q.oninput=()=>{clearTimeout(memberQueryTimer);memberQueryTimer=setTimeout(()=>act({action:'members_query',query:q.value}),250)};(m.picker.candidates||[]).forEach(c=>{const key=c.pubkey.slice(0,8)+'…';const b=lcButton(p,((m.picker.selected||[]).includes(c.pubkey)?'✓ ':'')+(c.label||key)+' · '+key,{action:'members_toggle',pubkey:c.pubkey});b.setAttribute('aria-label',`${c.label||key} ${c.pubkey}`)});const exact=lifecycleField(p,'Exact public key',previousExact);exact.dataset.exactKey='';const addExact=lcButton(p,'Add exact key',{action:'members_exact'});addExact.onclick=()=>act({action:'members_exact',pubkey:exact.value});const role=lifecycleField(p,'Role','','select');const prompt=document.createElement('option');prompt.value='';prompt.textContent='Select a role';prompt.selected=!m.picker.role_chosen;role.append(prompt);['owner','admin','member','guest','bot'].forEach(r=>{const o=document.createElement('option');o.value=r;o.textContent=r;o.selected=m.picker.role_chosen&&r===m.picker.role;role.append(o)});role.onchange=()=>{if(role.value)act({action:'members_role',role:role.value})};lcButton(p,'Confirm add',{action:'members_add_submit',community:state.community?.id},!(m.picker.selected||[]).length||!m.picker.role_chosen||m.picker.writing);(m.picker.results||[]).forEach(result=>{const line=document.createElement('p');line.textContent=`${result.pubkey.slice(0,8)}… · ${result.status}${result.error?' · '+result.error:''}`;p.append(line)})}}if(m.confirm){const c=document.createElement('p');c.className='notice';c.textContent=m.confirm.kind==='leave'?`Leave ${m.channel_name}? ${m.confirm.last_owner_warning?'You are the last owner; relay may refuse.':''}`:m.confirm.kind==='remove'?`Remove ${m.confirm.label} (${m.confirm.pubkey})?`:`Change ${m.confirm.label} (${m.confirm.pubkey}) to ${m.confirm.role}?`;p.append(c);lcButton(p,'Confirm',{action:'members_confirm',community:state.community?.id});lcButton(p,'Cancel',{action:'members_confirm_cancel'})}if(m.my_role)lcButton(p,'Leave channel',{action:'members_leave_open'});lcButton(p,'Close',{action:'members_close'});if(queryFocused&&q){q.focus({preventScroll:true});if(queryCaret!=null)try{q.setSelectionRange(queryCaret,queryCaret)}catch(_){} }else p.querySelector('button')?.focus()}
function renderLifecycleExtras(){if(!state)return;renderLifecycleHeader();document.querySelector('.filters')&&!document.querySelector('[data-filter="Archived"]')&&(()=>{const b=document.createElement('button');b.dataset.filter='Archived';b.textContent='Archived';b.onclick=()=>act({action:'filter',filter:'Archived'});document.querySelector('.filters').append(b)})();const f=state.channels?.find(x=>x.id===state.selected);document.querySelectorAll('.channel').forEach(b=>{const c=state.channels.find(x=>x.id===b.dataset.channel);if(c?.archived&&!b.querySelector('.archived-label')){const a=document.createElement('span');a.className='count archived-label';a.textContent='archived';b.append(a)}});if(state.composer_blocked){const t=document.querySelector('#composer'),s=document.querySelector('[data-act="send"]');if(t){t.disabled=true;t.placeholder=state.composer_blocked}if(s)s.disabled=true}renderEditLifecycle();renderArchiveLifecycle();renderMembersLifecycle()}
function renderArchiveLifecycle(){const a=state?.archive;if(!a){document.querySelector('.archive-modal')?.remove();archiveFingerprint='';return}const fp=JSON.stringify([a,state.community?.id]);if(fp===archiveFingerprint)return;archiveFingerprint=fp;const p=lcModal('archive-modal',a.unarchive?'Unarchive channel':'Archive channel',document.createElement('div'));const d=document.createElement('p');d.textContent=`${a.unarchive?'Restore':'Archive'} ${a.channel_name} in ${state.community?.name||'current community'}?`;p.append(d);if(a.failure){const e=document.createElement('p');e.className='error';e.textContent=a.failure;p.append(e)}lcButton(p,a.state==='creating'?'Working…':a.unarchive?'Unarchive':'Archive',{action:'archive_confirm',community:state.community?.id},a.state==='creating'||a.state==='unknown');if(a.state==='unknown')lcButton(p,'Refresh',{action:'archive_refresh'});lcButton(p,'Cancel',{action:'archive_close'});p.querySelector('button')?.focus()}
document.addEventListener('keydown',e=>{if(e.key!=='Escape')return;if(document.querySelector('.lifecycle-modal')){e.preventDefault();lifecycleModalClose();if(state?.edit_channel)act({action:'edit_channel_close'});else if(state?.members)act({action:'members_close'});else if(state?.archive)act({action:'archive_close'})}});
const lifecycleStyle=document.createElement('style');lifecycleStyle.textContent='.lifecycle-modal{position:fixed;inset:0;z-index:45;background:rgba(0,0,0,.55);display:flex;align-items:center;justify-content:center}.lifecycle-panel{background:var(--panel);border:1px solid var(--line);padding:18px;max-height:90vh;overflow:auto;width:min(560px,calc(100% - 24px));display:flex;flex-direction:column;gap:10px}.lifecycle-panel label{display:flex;flex-direction:column;gap:4px}.member-row{display:flex;align-items:center;gap:8px;justify-content:space-between;border-bottom:1px solid var(--line);padding:8px 0}.danger{color:var(--danger)}.archived-label{margin-left:auto}@media(max-width:899px){.lifecycle-panel{inset:0;width:100%;max-height:none;height:100%;border:0;border-radius:0}.desktop-only{display:none!important}}';document.head.append(lifecycleStyle);setInterval(renderLifecycleExtras,100);
</script></body></html>"##;

struct WebState {
    app: App,
    views: HashMap<String, App>,
    commands: mpsc::Sender<SessionCommand>,
    run_token: String,
    sessions: HashSet<String>,
    stopped: bool,
    startup_error: Option<String>,
    communities: Vec<Value>,
    active_community_id: Option<String>,
    transport_generation: u64,
}

#[derive(Debug, Deserialize)]
struct ActionRequest {
    action: String,
    channel: Option<String>,
    event: Option<String>,
    content: Option<String>,
    cursor: Option<usize>,
    delta: Option<i64>,
    /// The row a pointer selected in the mention popover.
    index: Option<i64>,
    query: Option<String>,
    filter: Option<String>,
    author: Option<String>,
    scope: Option<String>,
    time: Option<String>,
    community: Option<String>,
    visible: Option<bool>,
    latest: Option<String>,
    /// The create form's submitted fields. The browser owns its inputs and
    /// hands the whole form over in one request, so a tab never depends on
    /// what another tab typed.
    name: Option<String>,
    #[serde(rename = "type")]
    channel_type: Option<String>,
    visibility: Option<String>,
    description: Option<String>,
    /// The identity a membership action targets, as a canonical hex pubkey.
    /// The browser never shortens or resolves it: the session owns identity
    /// normalization.
    pubkey: Option<String>,
    /// The role an add or a role change applies: owner, admin, member, guest
    /// or bot, per the shared vocabulary.
    role: Option<String>,
}

#[derive(Debug)]
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

pub async fn run(resolved: Resolved) -> i32 {
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("buzzx: web listener failed: {error}");
            return config::EXIT_OTHER;
        }
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("buzzx: web listener address failed: {error}");
            return config::EXIT_OTHER;
        }
    };
    let link = format!(
        "http://127.0.0.1:{}/?access={}",
        address.port(),
        Uuid::new_v4()
    );
    let run_token = link.split("access=").nth(1).unwrap_or_default().to_owned();
    let (events_tx, mut events_rx) = mpsc::channel(256);
    let session = crate::session::spawn(&resolved, events_tx);
    let commands = session.commands.clone();
    let started = session.started;
    let finished = session.finished;
    let mut app = App::new(&resolved.keys, &resolved.http_url);
    if let Some(community) = &resolved.community {
        app.set_community(Some(&community.id), Some(&community.name));
    }
    let config_file = if config::config_path().is_file() {
        config::read_config_file(&config::config_path()).ok()
    } else {
        None
    };
    let communities = config_file
        .as_ref()
        .map(|file| {
            file.communities
                .iter()
                .map(|community| {
                    json!({
                        "id": community.id,
                        "name": community.name,
                        "relay_url": community.relay_url,
                        "last_channel": community.last_channel,
                        "active": false,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(file) = &config_file {
        app.set_communities(
            file.communities
                .iter()
                .map(|community| CommunityChoice {
                    id: community.id.clone(),
                    name: community.name.clone(),
                    relay_url: community.relay_url.clone(),
                    last_channel: community
                        .last_channel
                        .as_deref()
                        .and_then(|channel| Uuid::parse_str(channel).ok()),
                })
                .collect(),
        );
    }
    let active_community_id = resolved
        .community
        .as_ref()
        .map(|community| community.id.clone());
    let state = Arc::new(Mutex::new(WebState {
        app,
        views: HashMap::new(),
        commands: commands.clone(),
        run_token,
        sessions: HashSet::new(),
        stopped: false,
        startup_error: None,
        communities,
        active_community_id,
        transport_generation: 0,
    }));
    let start_state = Arc::clone(&state);
    tokio::spawn(async move {
        if let Ok(Err((_, reason))) = started.await {
            let mut guard = start_state.lock().await;
            guard.startup_error = Some(reason.clone());
            guard.app.status = reason;
        }
    });
    let event_state = Arc::clone(&state);
    let event_commands = commands.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                event = events_rx.recv() => {
                    let Some(event) = event else { break };
                    let outgoing = {
                        let mut guard = event_state.lock().await;
                        let now = now_secs();
                        let connected = current_connected_event(&mut guard, &event);
                        guard.app.apply(event.clone(), now);
                        if connected {
                            guard.active_community_id = guard.app.community_id.clone();
                            let active_id = guard.active_community_id.clone();
                            for profile in &mut guard.communities {
                                let active = profile
                                    .get("id")
                                    .and_then(Value::as_str)
                                    == active_id.as_deref();
                                profile["active"] = json!(active);
                            }
                        }
                        let mut outgoing = guard.app.take_outbox();
                        for view in guard.views.values_mut() {
                            view.apply(event.clone(), now);
                            outgoing.extend(view.take_outbox());
                        }
                        outgoing
                    };
                    send_commands(&event_commands, outgoing).await;
                }
                _ = tick.tick() => {
                    let mut guard = event_state.lock().await;
                    let now = now_secs();
                    guard.app.expire_typing(now);
                    guard.app.expire_agents(now);
                    for view in guard.views.values_mut() {
                        view.expire_typing(now);
                        view.expire_agents(now);
                    }
                }
            }
        }
    });
    println!("identity: {}", config::npub_short(&resolved.keys));
    println!("relay: {}", resolved.http_url);
    println!("web: {link}");
    println!("Ctrl+C to stop");
    open_browser(&link);

    let base = format!("http://127.0.0.1:{}", address.port());
    let result = loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        let state = Arc::clone(&state);
                        let base = base.clone();
                        tokio::spawn(async move { serve_connection(stream, peer, state, &base).await; });
                    }
                    Err(error) => eprintln!("buzzx: web accept failed: {error}"),
                }
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() { break 0; }
                break config::EXIT_OTHER;
            }
        }
    };
    {
        let mut guard = state.lock().await;
        guard.stopped = true;
    }
    let _ = commands.send(SessionCommand::Shutdown).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), finished).await;
    result
}

async fn send_commands(sender: &mpsc::Sender<SessionCommand>, commands: Vec<SessionCommand>) {
    for command in commands {
        if sender.send(command).await.is_err() {
            break;
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn open_browser(link: &str) {
    #[cfg(target_os = "linux")]
    let command = "xdg-open";
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(target_os = "windows")]
    let command = "cmd";
    #[cfg(target_os = "windows")]
    let args = ["/C", "start", "", link];
    #[cfg(not(target_os = "windows"))]
    let args = [link];
    let _ = std::process::Command::new(command).args(args).spawn();
}

async fn serve_connection(
    mut stream: TcpStream,
    _peer: std::net::SocketAddr,
    state: Arc<Mutex<WebState>>,
    base: &str,
) {
    let request = match read_request(&mut stream).await {
        Ok(request) => request,
        Err(error) => {
            let _ = response(
                &mut stream,
                400,
                "text/plain; charset=utf-8",
                error.to_string().as_bytes(),
                &[],
            )
            .await;
            return;
        }
    };
    let host_ok = header(&request, "host").is_some_and(|host| {
        host == &base[7..] || host == &base[7..].replace("127.0.0.1", "localhost")
    });
    if !host_ok {
        let _ = response(
            &mut stream,
            403,
            "text/plain; charset=utf-8",
            b"invalid host",
            &[],
        )
        .await;
        return;
    }
    if let Some(origin) = header(&request, "origin")
        && origin != base
        && origin != &base.replace("127.0.0.1", "localhost")
    {
        let _ = response(
            &mut stream,
            403,
            "text/plain; charset=utf-8",
            b"invalid origin",
            &[],
        )
        .await;
        return;
    }
    let (path, query) = split_target(&request.target);
    match (request.method.as_str(), path) {
        ("GET", "/") => bootstrap(&mut stream, query, state).await,
        ("GET", "/app") => {
            if authenticated(&request, &state).await {
                let _ = response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    HTML.as_bytes(),
                    &[],
                )
                .await;
            } else {
                let _ = response(
                    &mut stream,
                    401,
                    "text/plain; charset=utf-8",
                    b"session required",
                    &[],
                )
                .await;
            }
        }
        ("GET", "/api/state") => {
            if authenticated(&request, &state).await {
                let Some(session) = session_id(&request) else {
                    let _ = response(
                        &mut stream,
                        401,
                        "application/json",
                        br#"{"error":"session required"}"#,
                        &[],
                    )
                    .await;
                    return;
                };
                let body = snapshot(&state, &session).await;
                let _ = response(&mut stream, 200, "application/json", &body, &[]).await;
            } else {
                let _ = response(
                    &mut stream,
                    401,
                    "application/json",
                    br#"{"error":"session required"}"#,
                    &[],
                )
                .await;
            }
        }
        ("POST", "/api/action") => {
            if !authenticated(&request, &state).await {
                let _ = response(
                    &mut stream,
                    401,
                    "application/json",
                    br#"{"error":"session required"}"#,
                    &[],
                )
                .await;
            } else {
                let Some(session) = session_id(&request) else {
                    let _ = response(
                        &mut stream,
                        401,
                        "application/json",
                        br#"{"error":"session required"}"#,
                        &[],
                    )
                    .await;
                    return;
                };
                let body = match serde_json::from_slice::<ActionRequest>(&request.body) {
                    Ok(body) => body,
                    Err(error) => {
                        let _ = response(
                            &mut stream,
                            400,
                            "application/json",
                            json!({"error": error.to_string()}).to_string().as_bytes(),
                            &[],
                        )
                        .await;
                        return;
                    }
                };
                match action(&state, &session, body).await {
                    Ok(reply) => {
                        let body = json!({
                            "ok": true,
                            "view": reply.view,
                            "composer_text": reply.composer_text,
                            "composer_cursor": reply.composer_cursor,
                        })
                        .to_string();
                        let _ =
                            response(&mut stream, 200, "application/json", body.as_bytes(), &[])
                                .await;
                    }
                    Err(error) => {
                        let body = json!({"error": error}).to_string();
                        let _ =
                            response(&mut stream, 409, "application/json", body.as_bytes(), &[])
                                .await;
                    }
                }
            }
        }
        _ => {
            let _ = response(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"not found",
                &[],
            )
            .await;
        }
    }
}

async fn bootstrap(stream: &mut TcpStream, query: &str, state: Arc<Mutex<WebState>>) {
    let token = query_value(query, "access");
    let Some(token) = token else {
        let _ = response(
            stream,
            403,
            "text/plain; charset=utf-8",
            b"access link required",
            &[],
        )
        .await;
        return;
    };
    let session_id = Uuid::new_v4().to_string();
    let valid = {
        let mut guard = state.lock().await;
        if guard.stopped || guard.run_token != token {
            false
        } else {
            let view = guard.app.web_fork();
            guard.sessions.insert(session_id.clone());
            guard.views.insert(session_id.clone(), view);
            true
        }
    };
    if !valid {
        let _ = response(
            stream,
            403,
            "text/plain; charset=utf-8",
            b"invalid access link",
            &[],
        )
        .await;
        return;
    }
    let cookie = format!("{SESSION_COOKIE}={session_id}; HttpOnly; SameSite=Strict; Path=/");
    let headers = [("Location", "/app"), ("Set-Cookie", cookie.as_str())];
    let _ = response(
        stream,
        303,
        "text/plain; charset=utf-8",
        b"redirecting",
        &headers,
    )
    .await;
}

fn cookie_session(request: &Request) -> Option<String> {
    let cookie = header(request, "cookie")?;
    cookie.split(';').find_map(|part| {
        part.trim()
            .strip_prefix(&format!("{SESSION_COOKIE}="))
            .map(str::to_owned)
    })
}

fn session_id(request: &Request) -> Option<String> {
    let cookie = cookie_session(request)?;
    let tab = header(request, "x-buzzx-tab").filter(|value| !value.is_empty());
    Some(match tab {
        Some(tab) => format!("{cookie}:{tab}"),
        None => cookie,
    })
}

fn ensure_view(guard: &mut WebState, session: &str) -> bool {
    if guard.views.contains_key(session) {
        return true;
    }
    let Some(base) = session.split(':').next() else {
        return false;
    };
    if !guard.sessions.contains(base) {
        return false;
    }
    guard.views.insert(session.to_owned(), guard.app.web_fork());
    true
}

async fn authenticated(request: &Request, state: &Arc<Mutex<WebState>>) -> bool {
    let Some(session) = cookie_session(request) else {
        return false;
    };
    let guard = state.lock().await;
    !guard.stopped && guard.sessions.contains(&session)
}

/// One action's answer: the surface the tab should draw, and the composer the
/// session now holds. A picker insertion changes the draft, and the browser
/// owns its own text box, so the text comes back with the answer instead of
/// being re-derived from a projection the tab has not read yet.
#[derive(Debug)]
struct ActionReply {
    view: &'static str,
    composer_text: String,
    composer_cursor: usize,
}

async fn action(
    state: &Arc<Mutex<WebState>>,
    session: &str,
    request: ActionRequest,
) -> Result<ActionReply, String> {
    let (commands, reply, sender) = {
        let mut guard = state.lock().await;
        if guard.stopped {
            return Err("Session stopped".to_owned());
        }
        if !ensure_view(&mut guard, session) {
            return Err("session expired".to_owned());
        }
        let any_write_pending =
            guard.app.web_write_pending() || guard.views.values().any(App::web_write_pending);
        let pending_summary = guard
            .app
            .pending_write_summary()
            .or_else(|| guard.views.values().find_map(App::pending_write_summary));
        let app = guard
            .views
            .get_mut(session)
            .ok_or_else(|| "session expired".to_owned())?;
        let now = now_secs();
        let mut switch = None;
        match request.action.as_str() {
            "community" => {
                if any_write_pending {
                    return Err(match pending_summary {
                        Some(summary) => format!(
                            "a pending or uncertain write blocks community switching: {summary}"
                        ),
                        None => {
                            "a pending or uncertain write blocks community switching".to_owned()
                        }
                    });
                }
                let id = request
                    .community
                    .as_deref()
                    .ok_or_else(|| "community id is required".to_owned())?;
                let resolved =
                    config::resolve(None, None, None, Some(id)).map_err(|error| error.message)?;
                switch = Some(SessionCommand::SwitchCommunity(resolved));
                app.status = format!("switching to {id}");
            }
            "mention" => {
                app.web_mention_sync(
                    request.content.as_deref().unwrap_or_default(),
                    request.cursor.unwrap_or_default(),
                );
            }
            "mention_move" => {
                let delta = request.delta.unwrap_or(1);
                app.mention_move(delta.clamp(-1, 1) as isize);
            }
            "mention_select" => {
                // The pointer names the row it hit: a row-clicked selection
                // must land on that row, not one step from the cursor.
                if let Some(index) = request.index {
                    app.mention_select(index.max(0) as usize);
                }
            }
            "mention_confirm" => {
                app.mention_confirm();
            }
            "mention_close" => app.mention_dismiss(),
            "mention_retry" => app.retry_mention_roster(),
            "create_channel" => app.open_create_channel(),
            "create_channel_close" => {
                app.close_create_channel();
            }
            "create_channel_refresh" => app.refresh_create_channel(),
            "create_channel_submit" => {
                // The submit names the profile it was composed under and the
                // fields it shows: a tab whose community moved on is refused
                // here instead of creating in the wrong community. A run
                // started from a relay flag has no saved profile, and the page
                // sends the empty id it read, so the two compare equal there.
                if let Some(id) = request.community.as_deref()
                    && app.community_id.as_deref().unwrap_or_default() != id
                {
                    return Err(
                        "the active community changed; reopen the form and submit again".to_owned(),
                    );
                }
                apply_create_fields(app, &request)?;
                app.create_channel_submit();
            }
            "edit_channel" => app.open_edit_channel(),
            "edit_channel_close" => {
                app.close_edit_channel();
            }
            "edit_channel_refresh" => app.refresh_edit_channel(),
            "edit_channel_submit" => {
                ensure_community(app, &request)?;
                apply_edit_fields(app, &request)?;
                app.edit_channel_submit();
            }
            "archive" => {
                let channel = parse_channel(request.channel.as_deref())?;
                app.open_archive(channel);
            }
            "unarchive" => {
                let channel = parse_channel(request.channel.as_deref())?;
                app.open_unarchive(channel);
            }
            "archive_close" => app.close_archive(),
            "archive_confirm" => {
                ensure_community(app, &request)?;
                app.confirm_archive();
            }
            "archive_refresh" => app.refresh_archive(),
            "members" => {
                let channel = parse_channel(request.channel.as_deref())?;
                app.open_members(channel);
            }
            "members_close" => app.close_members(),
            "members_refresh" => app.refresh_members(),
            "members_add_open" => app.members_picker_open(),
            "members_add_close" => app.members_picker_close(),
            "members_query" => {
                app.members_picker_query(request.query.as_deref().unwrap_or_default())
            }
            "members_toggle" => {
                let pubkey = request.pubkey.as_deref().ok_or("pubkey is required")?;
                app.members_picker_toggle(pubkey);
            }
            "members_exact" => {
                let pubkey = request.pubkey.as_deref().ok_or("pubkey is required")?;
                app.members_picker_exact(pubkey)?;
            }
            "members_role" => {
                let role = parse_role(request.role.as_deref())?;
                app.members_picker_role(role);
            }
            "members_add_submit" => {
                ensure_community(app, &request)?;
                app.members_picker_submit();
            }
            "members_role_open" => {
                let pubkey = request.pubkey.as_deref().ok_or("pubkey is required")?;
                let role = parse_role(request.role.as_deref())?;
                app.members_role_open(pubkey, role);
            }
            "members_remove_open" => {
                let pubkey = request.pubkey.as_deref().ok_or("pubkey is required")?;
                app.members_remove_open(pubkey);
            }
            "members_leave_open" => app.members_leave_open(),
            "members_confirm" => {
                ensure_community(app, &request)?;
                app.members_confirm();
            }
            "members_confirm_cancel" => app.members_confirm_cancel(),
            "select" => {
                let id = parse_channel(request.channel.as_deref())?;
                app.web_select_channel(id)?;
            }
            "filter" => {
                if let Some(wanted) = request.filter.as_deref() {
                    for _ in 0..Filter::CYCLE.len() {
                        if app.filter.name() == wanted {
                            break;
                        }
                        app.handle(Action::FilterNext, now);
                    }
                } else {
                    app.handle(Action::FilterNext, now);
                }
            }
            "new" => app.handle(Action::ComposeNew, now),
            "reply" => {
                focus(app, request.event.as_deref())?;
                if app.thread.open {
                    app.handle(Action::ThreadReplyFocused, now);
                } else if app.context.open {
                    app.handle(Action::ContextReply, now);
                } else {
                    app.handle(Action::ComposeReply, now);
                }
            }
            "draft" => {
                if app.mode != Mode::Composer {
                    app.handle(Action::ComposeNew, now);
                }
                app.composer
                    .set_text(request.content.as_deref().unwrap_or_default());
            }
            "edit" => {
                focus(app, request.event.as_deref())?;
                app.handle(Action::EditRow, now);
            }
            "delete" => {
                focus(app, request.event.as_deref())?;
                app.handle(Action::DeleteRow, now);
            }
            "react" => {
                focus(app, request.event.as_deref())?;
                app.handle(Action::React, now);
            }
            "thread" => {
                focus(app, request.event.as_deref())?;
                if app.search.open {
                    app.handle(Action::SearchSubmit, now);
                } else if app.context.open {
                    app.handle(Action::ContextOpenThread, now);
                } else {
                    app.handle(Action::OpenThread, now);
                }
            }
            "context" => {
                focus(app, request.event.as_deref())?;
                if app.search.open {
                    app.handle(Action::SearchSubmit, now);
                }
            }
            "reader" => {
                focus(app, request.event.as_deref())?;
                if app.search.open {
                    app.handle(Action::SearchSubmit, now);
                } else if app.context.open {
                    app.handle(Action::ContextOpenReader, now);
                } else {
                    app.handle(Action::OpenReader, now);
                }
            }
            "send" => {
                if let Some(event) = request.event.as_deref() {
                    focus(app, Some(event))?;
                    if app.thread.open {
                        app.handle(Action::ThreadReplyFocused, now);
                    } else if app.context.open {
                        app.handle(Action::ContextReply, now);
                    } else {
                        app.handle(Action::ComposeReply, now);
                    }
                } else if app.mode != Mode::Composer {
                    app.handle(Action::ComposeNew, now);
                }
                app.composer
                    .set_text(request.content.as_deref().unwrap_or_default());
                app.handle(Action::ComposerSend, now);
            }
            "search" => {
                app.handle(Action::OpenSearch, now);
                app.search.query = request.query.unwrap_or_default();
                app.search.author = request.author.filter(|value| !value.trim().is_empty());
                app.search.scope = match request.scope.as_deref() {
                    Some("all") => SearchScope::All,
                    Some(raw) => Uuid::parse_str(raw)
                        .map(SearchScope::Conversation)
                        .unwrap_or(SearchScope::Current),
                    None => SearchScope::Current,
                };
                app.search.time = match request.time.as_deref() {
                    Some("7d") => SearchTime::Days7,
                    Some("30d") => SearchTime::Days30,
                    _ => SearchTime::All,
                };
                app.search.editing = true;
                app.handle(Action::SearchSubmit, now);
            }
            "older" => match app.surface() {
                crate::keys::Surface::Context => app.handle(Action::ContextLoadOlder, now),
                crate::keys::Surface::Thread => app.handle(Action::ContextLoadOlder, now),
                _ => app.handle(Action::PageUp, now),
            },
            "newer" => match app.surface() {
                crate::keys::Surface::Context => app.handle(Action::ContextLoadNewer, now),
                crate::keys::Surface::Thread => app.handle(Action::ContextLoadNewer, now),
                _ => app.handle(Action::PageDown, now),
            },
            "latest" => app.handle(Action::Bottom, now),
            "agents" => app.handle(Action::ToggleAgents, now),
            "help" => app.handle(Action::ToggleHelp, now),
            "back" => {
                if app.reader.open {
                    app.handle(Action::ReaderClose, now);
                } else if app.context.open {
                    app.handle(Action::ContextLeave, now);
                } else if app.thread.open {
                    app.handle(Action::ThreadLeave, now);
                } else if app.search.open || app.agents.open || app.help {
                    app.handle(Action::Dismiss, now);
                }
            }
            "presented" => {
                if request.visible == Some(true) {
                    let current = app
                        .channels
                        .get(app.selected)
                        .and_then(|entry| entry.rows.last())
                        .map(|row| row.event_id.as_str());
                    if current.is_some() && current == request.latest.as_deref() {
                        app.note_presented();
                    }
                }
            }
            other => return Err(format!("unknown action: {other}")),
        }
        let view = view_for(app);
        let reply = ActionReply {
            view,
            composer_text: app.composer.text(),
            composer_cursor: app.composer.offset(),
        };
        let mut commands = app.take_outbox();
        if let Some(switch) = switch {
            commands.push(switch);
        }
        let sender = guard.commands.clone();
        (commands, reply, sender)
    };
    send_commands(&sender, commands).await;
    Ok(reply)
}
fn ensure_community(app: &App, request: &ActionRequest) -> Result<(), String> {
    if let Some(id) = request.community.as_deref()
        && app.community_id.as_deref().unwrap_or_default() != id
    {
        return Err("the active community changed; reopen the form and submit again".to_owned());
    }
    Ok(())
}

fn parse_role(raw: Option<&str>) -> Result<MemberRole, String> {
    raw.ok_or_else(|| "role is required".to_owned())?
        .parse()
        .map_err(|_| "unsupported role".to_owned())
}

fn apply_edit_fields(app: &mut App, request: &ActionRequest) -> Result<(), String> {
    let Some(form) = app.edit_channel.as_mut() else {
        return Err("no channel edit is open".to_owned());
    };
    if let Some(name) = request.name.as_deref() {
        form.name = name.to_owned();
    }
    if let Some(description) = request.description.as_deref() {
        form.description = description.to_owned();
    }
    if let Some(visibility) = request.visibility.as_deref() {
        form.visibility = visibility
            .parse::<buzz_core::channel::ChannelVisibility>()
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Apply one submit's fields onto the form the session owns. The browser
/// reads the form back from the projection, so the values it sends are the
/// values it showed, and the session's copy is what the preflight sees.
fn apply_create_fields(app: &mut App, request: &ActionRequest) -> Result<(), String> {
    let kind = match request.channel_type.as_deref() {
        Some("stream") => Some(buzz_core::channel::ChannelType::Stream),
        Some("forum") => Some(buzz_core::channel::ChannelType::Forum),
        Some(raw) => return Err(format!("unsupported channel type: {raw}")),
        None => None,
    };
    let visibility = match request.visibility.as_deref() {
        Some(raw) => Some(raw.parse::<buzz_core::channel::ChannelVisibility>()?),
        None => None,
    };
    let Some(form) = app.create_channel.as_mut() else {
        return Err("no channel creation is open".to_owned());
    };
    if let Some(name) = request.name.as_deref() {
        form.name = name.to_owned();
    }
    if let Some(kind) = kind {
        form.kind = kind;
    }
    if let Some(visibility) = visibility {
        form.visibility = visibility;
    }
    if let Some(description) = request.description.as_deref() {
        form.description = description.to_owned();
    }
    Ok(())
}

fn focus(app: &mut App, event: Option<&str>) -> Result<(), String> {
    let Some(event) = event else {
        return Err("a confirmed event is required".to_owned());
    };
    if app.thread.open {
        let Some(index) = app.thread.rows.iter().position(|row| row.event_id == event) else {
            return Err("event is not loaded".to_owned());
        };
        app.thread.focus = index;
    } else if app.context.open {
        let Some(index) = app
            .context
            .rows
            .iter()
            .position(|row| row.event_id == event)
        else {
            return Err("event is not loaded".to_owned());
        };
        app.context.focus = index;
    } else if app.search.open {
        let Some(index) = app
            .search
            .results
            .iter()
            .position(|row| row.event_id == event)
        else {
            return Err("event is not loaded".to_owned());
        };
        app.search.focus = index;
    } else {
        let Some(index) = app
            .channels
            .get(app.selected)
            .and_then(|entry| entry.rows.iter().position(|row| row.event_id == event))
        else {
            return Err("event is not loaded".to_owned());
        };
        app.focus = index;
    }
    Ok(())
}

fn parse_channel(raw: Option<&str>) -> Result<Uuid, String> {
    raw.ok_or_else(|| "channel is required".to_owned())
        .and_then(|raw| Uuid::parse_str(raw).map_err(|_| "invalid channel".to_owned()))
}

fn view_for(app: &App) -> &'static str {
    if app.reader.open {
        "reader"
    } else if app.context.open {
        "context"
    } else if app.search.open {
        "search"
    } else if app.thread.open {
        "thread"
    } else if app.agents.open {
        "agents"
    } else if app.help {
        "help"
    } else {
        "channel"
    }
}

fn current_connected_event(state: &mut WebState, event: &crate::session::ChatEvent) -> bool {
    match event {
        crate::session::ChatEvent::Generation { generation, event } => {
            if *generation < state.transport_generation {
                return false;
            }
            state.transport_generation = *generation;
            is_connected_event(event)
        }
        crate::session::ChatEvent::CommunitySwitching { generation, .. } => {
            if *generation >= state.transport_generation {
                state.transport_generation = *generation;
            }
            false
        }
        _ => is_connected_event(event),
    }
}

fn is_connected_event(event: &crate::session::ChatEvent) -> bool {
    match event {
        crate::session::ChatEvent::Connected => true,
        crate::session::ChatEvent::Generation { event, .. } => is_connected_event(event),
        _ => false,
    }
}

async fn snapshot(state: &Arc<Mutex<WebState>>, session: &str) -> Vec<u8> {
    let mut guard = state.lock().await;
    if !ensure_view(&mut guard, session) {
        return br#"{"error":"session expired"}"#.to_vec();
    }
    let Some(app) = guard.views.get(session) else {
        return br#"{"error":"session expired"}"#.to_vec();
    };
    let mention_suggestions = app.mention_picker.as_ref().map(|picker| {
        json!({
            "query": picker.active.query,
            "items": app.mention_items().into_iter().map(|candidate| json!({
                "pubkey": candidate.pubkey,
                "label": candidate.label,
                "picture": candidate.picture,
                "short_key": candidate.short_key(),
                "agent": candidate.agent,
                "admin": candidate.admin,
            })).collect::<Vec<_>>(),
            "selected_index": picker.cursor,
            "loading": app.mention_loading(),
            "failed": app.mention_failure(),
            // The roster belongs to one conversation under one session: the
            // browser compares these before it draws a suggestion.
            "community_id": app.community_id,
            "channel_id": picker.channel,
            "generation": guard.transport_generation,
        })
    });
    let mention_block = app.mention_block.as_ref().map(|block| {
        json!({
            "kind": block.kind,
            "summary": block.summary,
            "details": block.details,
            // The refusal's own session, not whatever the tab reads now: an
            // old block is never relabelled as the current community's.
            "community_id": block.community,
            "channel_id": block.channel,
            "generation": block.generation,
        })
    });
    let create_channel = app.create_channel.as_ref().map(|form| {
        let (state_name, failure) = match &form.state {
            CreateState::Editing => ("editing", None),
            CreateState::Creating => ("creating", None),
            CreateState::Failed(reason) => ("failed", Some(reason.as_str())),
            CreateState::Unknown(reason) => ("unknown", Some(reason.as_str())),
        };
        json!({
            "name": form.name,
            "type": form.kind.as_str(),
            "visibility": form.visibility.as_str(),
            "description": form.description,
            "field": CreateChannelForm::label(form.field),
            "state": state_name,
            "ready": form.ready(),
            "pending": app.create_write_pending(),
            "failure": failure,
        })
    });
    // The write that outlives the form: the page keeps offering the refresh
    // that settles it after the modal has been dismissed.
    let create_unsettled = app.create_unsettled.as_ref().map(|open| {
        let state_name = match &open.state {
            CreateState::Unknown(_) => "unknown",
            _ => "creating",
        };
        json!({
            "name": open.draft.name,
            "type": open.draft.kind.as_str(),
            "visibility": open.draft.visibility.as_str(),
            "state": state_name,
        })
    });
    let channel_details = app.channel_details().map(|details| {
        json!({
            "name": details.name, "description": details.description,
            "visibility": details.visibility.map(|v| v.as_str()),
            "kind": channel_kind(details.kind), "archived": details.archived,
            "my_role": details.my_role.map(|r| r.as_str()),
        })
    });
    let lifecycle_state = |state: &CreateState| match state {
        CreateState::Editing => "editing",
        CreateState::Creating => "creating",
        CreateState::Failed(_) => "failed",
        CreateState::Unknown(_) => "unknown",
    };
    let edit_channel = app.edit_channel.as_ref().map(|form| json!({
        "channel": form.channel, "name": form.name, "description": form.description,
        "visibility": form.visibility.as_str(), "state": lifecycle_state(&form.state),
        "failure": match &form.state { CreateState::Failed(e) | CreateState::Unknown(e) => Some(e), _ => None },
    }));
    let archive = app.archive.as_ref().map(|form| json!({
        "channel": form.channel, "channel_name": form.channel_name, "unarchive": form.unarchive,
        "state": lifecycle_state(&form.state),
        "failure": match &form.state { CreateState::Failed(e) | CreateState::Unknown(e) => Some(e), _ => None },
    }));
    let members = app.members.as_ref().map(|panel| {
        let rows = panel
            .members
            .iter()
            .map(|row| {
                json!({
                    "pubkey": row.pubkey, "label": row.label, "role": row.role.map(|r| r.as_str()),
                    "agent": row.agent, "is_self": row.is_self, "last_owner": row.last_owner,
                })
            })
            .collect::<Vec<_>>();
        let candidates = panel
            .picker
            .candidates
            .iter()
            .map(|candidate| {
                json!({
                    "pubkey": candidate.pubkey, "label": candidate.label, "agent": candidate.agent,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "channel": panel.channel, "channel_name": panel.channel_name, "loading": panel.loading,
            "failed": panel.failed, "members": rows, "my_role": panel.my_role.map(|r| r.as_str()),
            "generation": panel.generation, "complete": panel.complete,
            "picker": {"open": panel.picker.open, "query": panel.picker.query,
                "loading": panel.picker.loading, "failed": panel.picker.failed,
                "candidates": candidates, "selected": panel.picker.selected,
                "role": panel.picker.role.as_str(), "role_chosen": panel.picker.role_chosen,
                "results": panel.picker.results.iter().map(|result| json!({
                    "pubkey": result.pubkey, "status": format!("{:?}", result.status),
                    "error": result.error,
                })).collect::<Vec<_>>()},
            "confirm": panel.confirm.as_ref().map(|confirm| match confirm {
                crate::app::MemberConfirm::Remove { pubkey, label } =>
                    json!({"kind":"remove","pubkey":pubkey,"label":label}),
                crate::app::MemberConfirm::ChangeRole { pubkey, label, role } =>
                    json!({"kind":"role","pubkey":pubkey,"label":label,"role":role.as_str()}),
                crate::app::MemberConfirm::Leave { last_owner_warning } =>
                    json!({"kind":"leave","last_owner_warning":last_owner_warning}),
            }),
        })
    });
    let selected = app
        .channels
        .get(app.selected)
        .map(|entry| entry.id.to_string());
    let channels: Vec<Value> = app.channels.iter().map(|entry| json!({
        "id": entry.id.to_string(), "name": app.label(entry), "kind": channel_kind(entry.kind),
        "description": entry.description, "visibility": entry.visibility.as_ref().map(|v| v.as_str()),
        "my_role": entry.my_role.as_ref().map(|r| r.as_str()), "archived": entry.archived,
        "matched": app.filter.matches(entry),
        "unread": app.unread_count(entry), "marker": marker(app.marker(entry)), "typing": app.is_typing(entry.id, now_secs()),
    })).collect();
    let rows = app
        .channels
        .get(app.selected)
        .map(|entry| {
            entry
                .rows
                .iter()
                .map(|row| row_json(row, &app.me))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let thread = if app.thread.open {
        app.thread
            .rows
            .iter()
            .map(|row| row_json(row, &app.me))
            .collect()
    } else {
        Vec::new()
    };
    let context = if app.context.open {
        app.context
            .rows
            .iter()
            .map(|row| row_json(row, &app.me))
            .collect()
    } else {
        Vec::new()
    };
    let search = app
        .search
        .results
        .iter()
        .map(|row| row_json(row, &app.me))
        .collect::<Vec<_>>();
    let reader = app.reader.row.as_ref().map(|row| row_json(row, &app.me));
    let agents: Vec<Value> = match &app.agents.roster {
        AgentLoad::Loaded(roster) => roster
            .agents
            .iter()
            .map(|agent| {
                let contexts = app
                    .agent_contexts(&agent.pubkey)
                    .into_iter()
                    .map(|context| match context {
                        Context::Channel { id, name } => json!({"kind":"channel","id":id,"name":name}),
                        Context::Unavailable => json!({"kind":"unavailable"}),
                        Context::Unknown => json!({"kind":"unknown"}),
                    })
                    .collect::<Vec<_>>();
                json!({"name":agent.name,"pubkey":agent.pubkey,"status":agent_status(app, &agent.pubkey),"contexts":contexts})
            })
            .collect(),
        _ => Vec::new(),
    };
    let title = app
        .channels
        .get(app.selected)
        .map(|entry| app.label(entry))
        .unwrap_or_else(|| "Inbox".to_owned());
    let mut body = Map::new();
    body.insert("identity".to_owned(), json!(app.me));
    body.insert("relay".to_owned(), json!(app.relay_label));
    body.insert("generation".to_owned(), json!(guard.transport_generation));
    body.insert(
        "community".to_owned(),
        json!({"id": app.community_id, "name": app.community_name}),
    );
    body.insert("communities".to_owned(), json!(&guard.communities));
    body.insert("connection".to_owned(), json!(connection(app.conn)));
    body.insert("status".to_owned(), json!(app.status));
    body.insert("startup_error".to_owned(), json!(guard.startup_error));
    body.insert("selected".to_owned(), json!(selected));
    body.insert("title".to_owned(), json!(title));
    body.insert("channels".to_owned(), json!(channels));
    body.insert("rows".to_owned(), json!(rows));
    body.insert("focus".to_owned(), json!(app.focus));
    body.insert("thread".to_owned(), json!(thread));
    body.insert("thread_focus".to_owned(), json!(app.thread.focus));
    body.insert("thread_loading".to_owned(), json!(app.thread.loading));
    body.insert("thread_failed".to_owned(), json!(app.thread.failed));
    body.insert("context".to_owned(), json!(context));
    body.insert("context_focus".to_owned(), json!(app.context.focus));
    body.insert("context_loading".to_owned(), json!(app.context.loading));
    body.insert("context_failed".to_owned(), json!(app.context.failed));
    body.insert("search".to_owned(), json!(search));
    body.insert("search_query".to_owned(), json!(app.search.query));
    body.insert("search_focus".to_owned(), json!(app.search.focus));
    body.insert(
        "search_applied_query".to_owned(),
        json!(app.search.applied_query),
    );
    body.insert(
        "search_applied_author".to_owned(),
        json!(app.search.applied_author),
    );
    body.insert("search_hidden".to_owned(), json!(app.search.hidden));
    body.insert(
        "search_scope".to_owned(),
        json!(match app.search.scope {
            SearchScope::Current => "current".to_owned(),
            SearchScope::All => "all".to_owned(),
            SearchScope::Conversation(id) => id.to_string(),
        }),
    );
    body.insert("search_author".to_owned(), json!(app.search.author));
    body.insert(
        "search_time".to_owned(),
        json!(match app.search.time {
            SearchTime::All => "all",
            SearchTime::Days7 => "7d",
            SearchTime::Days30 => "30d",
        }),
    );
    body.insert("search_loading".to_owned(), json!(app.search.loading));
    body.insert("search_failed".to_owned(), json!(app.search.failed));
    body.insert("search_bounded".to_owned(), json!(app.search.bounded));
    body.insert("search_previous".to_owned(), json!(app.search.previous));
    body.insert("reader".to_owned(), json!(reader));
    body.insert("agents".to_owned(), json!(agents));
    body.insert("composer_label".to_owned(), json!(composer_label(app)));
    body.insert("composer_text".to_owned(), json!(app.composer.text()));
    body.insert(
        "composer_mode".to_owned(),
        json!(app.mode == Mode::Composer),
    );
    body.insert("write_pending".to_owned(), json!(app.web_write_pending()));
    body.insert("composer_blocked".to_owned(), json!(app.composer_blocked()));
    body.insert("channel_details".to_owned(), json!(channel_details));
    body.insert("edit_channel".to_owned(), json!(edit_channel));
    body.insert("archive".to_owned(), json!(archive));
    body.insert("members".to_owned(), json!(members));
    body.insert(
        "role_chosen".to_owned(),
        json!(
            members
                .as_ref()
                .and_then(|m| m.get("picker"))
                .and_then(|p| p.get("role_chosen"))
        ),
    );
    body.insert("mention_suggestions".to_owned(), json!(mention_suggestions));
    body.insert("mention_block".to_owned(), json!(mention_block));
    body.insert("create_channel".to_owned(), json!(create_channel));
    body.insert("create_unsettled".to_owned(), json!(create_unsettled));
    body.insert("surface".to_owned(), json!(view_for(app)));
    let mut body = Value::Object(body);
    body["filter"] = json!(app.filter.name());
    body["filter_empty"] = json!(app.empty_view());
    serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec())
}

fn row_json(row: &Row, me: &str) -> Value {
    json!({"event_id":row.event_id,"pubkey":row.pubkey,"author":row.author,"created_at":row.created_at,"body":row.body,"root_id":row.root_id,"parent_id":row.parent_id,"mentions_me":row.mentions_me,"reactions":row.reactions,"attachment":row.attachment,"pending":row.pending,"uncertain":row.uncertain,"edited":row.edited,"own":row.pubkey==me})
}

fn composer_label(app: &App) -> String {
    if let Some(target) = &app.composer.edit {
        return format!("Edit your message ({})", target.event_id);
    }
    if let Some(target) = &app.composer.reply {
        return format!("Reply to {}", target.author);
    }
    "New message".to_owned()
}

fn agent_status(app: &App, pubkey: &str) -> String {
    match app.agent_status(pubkey, now_secs()) {
        AgentStatus::Working(count) => format!("working ({count})"),
        AgentStatus::Typing(channel) => format!("typing in {channel}"),
        AgentStatus::NoTurn => "no active turn".to_owned(),
        AgentStatus::Unknown => "unknown".to_owned(),
    }
}

fn channel_kind(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Channel => "channel",
        ChannelKind::Dm => "dm",
        ChannelKind::Unknown => "unknown",
    }
}
fn marker(marker: crate::app::Marker) -> &'static str {
    match marker {
        crate::app::Marker::Unread => "unread",
        crate::app::Marker::Mention => "mention",
        crate::app::Marker::Unknown => "unknown",
        crate::app::Marker::Read => "read",
        crate::app::Marker::None => "none",
    }
}
fn connection(conn: ConnState) -> &'static str {
    match conn {
        ConnState::Connecting => "Connecting",
        ConnState::Connected => "Connected",
        ConnState::Reconnecting => "Reconnecting",
    }
}

fn split_target(target: &str) -> (&str, &str) {
    target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query))
}
fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key == name).then(|| value.to_owned())
    })
}
fn header<'a>(request: &'a Request, name: &str) -> Option<&'a String> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

async fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut buffer = Vec::with_capacity(4096);
    let split;
    loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "empty request",
            ));
        }
        buffer.extend_from_slice(&chunk[..count]);
        if buffer.len() > MAX_REQUEST {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            split = index;
            break;
        }
    }
    let head = std::str::from_utf8(&buffer[..split])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid headers"))?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_owned();
    let target = first.next().unwrap_or_default().to_owned();
    if method.is_empty() || target.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request line",
        ));
    }
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect();
    let length = header_from(&headers, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_REQUEST {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }
    let mut body = buffer[split + 4..].to_vec();
    while body.len() < length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated body",
            ));
        }
        body.extend_from_slice(&chunk[..count]);
    }
    body.truncate(length);
    Ok(Request {
        method,
        target,
        headers,
        body,
    })
}
fn header_from<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

async fn response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, &str)],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    let mut output = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'\r\nConnection: close\r\n",
        body.len()
    );
    for (key, value) in extra {
        output.push_str(&format!("{key}: {value}\r\n"));
    }
    output.push_str("\r\n");
    output.push_str(std::str::from_utf8(body).unwrap_or_default());
    stream.write_all(output.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_query_requires_the_exact_parameter() {
        assert_eq!(
            query_value("foo=bar&access=token-1", "access"),
            Some("token-1".to_owned())
        );
        assert_eq!(query_value("accessibility=token-1", "access"), None);
    }

    #[test]
    fn split_target_keeps_path_separate_from_query() {
        assert_eq!(split_target("/?access=token"), ("/", "access=token"));
        assert_eq!(split_target("/api/state"), ("/api/state", ""));
    }

    #[test]
    fn hostile_body_is_not_interpreted_as_markup_by_the_row_projection() {
        let row = Row {
            event_id: "event".to_owned(),
            pubkey: "author".to_owned(),
            author: "<script>alert(1)</script>".to_owned(),
            created_at: 1,
            body: "<img src=x onerror=alert(1)>".to_owned(),
            kind: 9,
            root_id: None,
            parent_id: None,
            broadcast: true,
            mentions_me: false,
            reactions: Vec::new(),
            attachment: None,
            pending: false,
            uncertain: false,
            edited: false,
        };
        let value = row_json(&row, "me");
        assert_eq!(value["body"], "<img src=x onerror=alert(1)>");
        assert_eq!(value["author"], "<script>alert(1)</script>");
    }
    #[test]
    fn mention_and_channel_actions_parse_their_payloads() {
        let request: ActionRequest =
            serde_json::from_str(r#"{"action":"mention","content":"@Zoë","cursor":5}"#)
                .expect("mention request");
        assert_eq!(request.action, "mention");
        assert_eq!(request.cursor, Some(5));
        let request: ActionRequest = serde_json::from_str(
            r#"{"action":"create_channel_submit","community":"work","name":"项目讨论","type":"forum","visibility":"private","description":"可选说明"}"#,
        )
        .expect("channel submit request");
        assert_eq!(request.community.as_deref(), Some("work"));
        assert_eq!(request.name.as_deref(), Some("项目讨论"));
        assert_eq!(request.channel_type.as_deref(), Some("forum"));
        assert_eq!(request.visibility.as_deref(), Some("private"));
        assert_eq!(request.description.as_deref(), Some("可选说明"));
    }

    fn web_state() -> Arc<Mutex<WebState>> {
        let (commands, receiver) = mpsc::channel(8);
        drop(receiver);
        Arc::new(Mutex::new(WebState {
            app: App::new(&nostr::Keys::generate(), "http://relay.test"),
            views: HashMap::new(),
            commands,
            run_token: "token".to_owned(),
            sessions: HashSet::from(["tab".to_owned()]),
            stopped: false,
            startup_error: None,
            communities: Vec::new(),
            active_community_id: None,
            transport_generation: 0,
        }))
    }

    /// The page names the community it read from the snapshot, and a run
    /// started from a relay flag has no saved profile: the empty id must not
    /// read as "the tab moved on" and refuse the submit.
    #[tokio::test]
    async fn a_submit_without_a_saved_profile_is_not_a_community_change() {
        let state = web_state();
        let open: ActionRequest =
            serde_json::from_str(r#"{"action":"create_channel"}"#).expect("open request");
        action(&state, "tab:1", open).await.expect("the form opens");
        let request: ActionRequest = serde_json::from_str(
            r#"{"action":"create_channel_submit","community":"","name":"项目讨论","type":"stream","visibility":"open","description":""}"#,
        )
        .expect("channel submit request");
        action(&state, "tab:1", request)
            .await
            .expect("a submit that names the active profile is not a community change");
        let guard = state.lock().await;
        let form = guard
            .views
            .get("tab:1")
            .and_then(|view| view.create_channel.as_ref())
            .expect("the form stays open while its write is in flight");
        assert_eq!(form.name, "项目讨论");
        assert_eq!(form.state, CreateState::Creating);
    }

    /// A tab that composed under another profile is still refused.
    #[tokio::test]
    async fn a_submit_naming_another_profile_is_refused() {
        let state = web_state();
        {
            let mut guard = state.lock().await;
            guard.app.open_create_channel();
        }
        let request: ActionRequest = serde_json::from_str(
            r#"{"action":"create_channel_submit","community":"work","name":"work notes","type":"stream","visibility":"open","description":""}"#,
        )
        .expect("channel submit request");
        let refusal = action(&state, "tab:1", request)
            .await
            .expect_err("a submit from another profile is refused");
        assert_eq!(
            refusal,
            "the active community changed; reopen the form and submit again"
        );
    }
}
