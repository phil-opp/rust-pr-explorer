"use strict";
// Rust PR Explorer front end. All data is embedded in the page as JSON.

const D = JSON.parse(document.getElementById("data").textContent);
const items = D.items;
const ROOTS = D.crates;
const SINGLE_CRATE = ROOTS.length === 1;
const $ = (sel, el = document) => el.querySelector(sel);
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

// ---------------------------------------------------------------------------
// Derived data

for (const it of Object.values(items)) {
  it.callers = [];
  it.usedBy = [];
}
for (const it of Object.values(items)) {
  for (const c of it.calls) items[c.to]?.callers.push({ from: it.id, inferred: !!c.inferred });
  for (const u of it.uses) items[u.to]?.usedBy.push({ from: it.id, ctx: u.ctx });
}

const isRoot = (it) => it.kind === "mod" && it.module === it.id;
const modOf = (it) => (it.kind === "mod" ? it.id : it.module);
const within = (m, s) => m === s || m.startsWith(s + "::");
const changed = (it) => it.status !== "unchanged";
const MEMBER_KINDS = new Set(["method", "trait_method", "assoc_const", "assoc_type"]);
const TYPE_KINDS = new Set(["struct", "enum", "union", "trait", "type_alias"]);

/** Ancestors of a module id, outermost first, including itself. */
function modChain(m) {
  const out = [];
  let cur = items[m];
  while (cur) {
    out.unshift(cur.id);
    if (isRoot(cur)) break;
    cur = items[cur.module];
  }
  return out;
}

const modDepth = (m) => modChain(m).length - 1;

/** Items listed directly in a module (not associated items). */
function moduleChildren(m) {
  return Object.values(items).filter((it) => it.module === m && it.id !== m && !it.parent);
}

/** Members of a type, trait or impl. */
function membersOf(it) {
  return it.children.map((c) => items[c]).filter(Boolean);
}

/** Whether `it` or anything it contains was changed. */
const touchedMemo = new Map();
function touched(it) {
  if (touchedMemo.has(it.id)) return touchedMemo.get(it.id);
  touchedMemo.set(it.id, false);
  let r = changed(it) && !(it.kind === "mod" && it.status === "unchanged");
  if (!r && it.kind === "mod") r = moduleChildren(it.id).some(touched);
  if (!r && it.kind !== "mod") r = membersOf(it).some((m) => m.id !== it.id && touched(m));
  touchedMemo.set(it.id, r);
  return r;
}

/** Changed items (not modules) defined in a module or its descendants. */
function changesIn(m, deep = true) {
  return Object.values(items).filter(
    (it) => it.kind !== "mod" && changed(it) && (deep ? within(modOf(it), m) : modOf(it) === m),
  );
}

/**
 * Whether item `id` can be named from code in module `scope`, following
 * Rust's privacy rules: the item and every enclosing module up to a common
 * ancestor must be visible there.
 */
function visibleFrom(id, scope) {
  let x = items[id];
  let guard = 0;
  while (x && guard++ < 50) {
    if (x.kind === "mod" && within(scope, x.id)) return true;
    if (x.vis_scope && !within(scope, x.vis_scope)) return false;
    if (!x.vis_parent) return true;
    x = items[x.vis_parent];
  }
  return true;
}

/** The module that hides `id` from `scope` (where one would drill in). */
function hidingModule(id, scope) {
  let x = items[id];
  let hide = null;
  while (x) {
    if (x.kind === "mod" && within(scope, x.id)) break;
    if (x.vis_scope && !within(scope, x.vis_scope)) hide = x.vis_scope;
    if (!x.vis_parent) break;
    x = items[x.vis_parent];
  }
  return hide;
}

/** Private to module m: not visible outside it. */
const privateTo = (it, m) => !!it.vis_scope && within(it.vis_scope, m);

function shortPath(id) {
  if (!SINGLE_CRATE) return id;
  const root = ROOTS[0];
  if (id === root) return root;
  return id.startsWith(root + "::") ? id.slice(root.length + 2) : id;
}

function displayName(it) {
  if (it.kind === "mod" || it.kind === "impl") return it.name;
  const parent = it.parent && items[it.parent];
  if (parent) {
    if (parent.kind === "impl") {
      const self = items[parent.self_ty]?.name ?? "Self";
      const tr = (parent.trait_ref?.path ?? "").split("::").pop();
      return `<${self} as ${tr}>::${it.name}`;
    }
    return `${parent.name}::${it.name}`;
  }
  return it.name;
}

function modLabel(m) {
  const it = items[m];
  if (!it) return m;
  return isRoot(it) ? `crate ${it.name}` : `mod ${shortPath(m)}`;
}

function kindLabel(k) {
  return { trait_method: "method", type_alias: "type", assoc_const: "const", assoc_type: "type", impl: "impl" }[k] || k;
}

function titleHtml(it, withModule = false) {
  const prefix = withModule && it.kind !== "mod" && !isRoot(items[it.module] || {}) ? `<span class="dim">${esc(shortPath(it.module))}::</span>` : "";
  return `<span class="title-name">${prefix}<span class="k-${it.kind}">${esc(displayName(it))}</span></span>`;
}

function statusHtml(it) {
  if (it.kind === "mod") {
    if (it.status === "added") return `<span class="st st-added">+ new module</span>`;
    if (it.status === "removed") return `<span class="st st-removed">− removed</span>`;
    const n = changesIn(it.id).length;
    return n ? `<span class="st st-inner">${n} changed</span>` : "";
  }
  const c = it.changes;
  switch (it.status) {
    case "added": return `<span class="st st-added">+ added</span>`;
    case "removed": return `<span class="st st-removed">− removed</span>`;
    case "changed": {
      const what = [c.signature && "signature", c.body && "body", c.docs && "docs"].filter(Boolean);
      let s = `<span class="st st-changed">~ ${what.length ? what.join(", ") : "changed"}</span>`;
      if (c.moved_from) s += ` <span class="st st-changed" title="${esc(c.moved_from)}">moved from ${esc(shortPath(c.moved_from.split("::").slice(0, -1).join("::")))}</span>`;
      return s;
    }
    default:
      if (c.cosmetic) return `<span class="st st-cosmetic" title="Only formatting, comments or import style differ">formatting only</span>`;
      if (membersOf(it).some((m) => touched(m))) return `<span class="st st-inner">members changed</span>`;
      return `<span class="st st-unchanged">unchanged</span>`;
  }
}

function visHtml(it) {
  if (MEMBER_KINDS.has(it.kind) && it.parent && items[it.parent]?.kind === "impl") return "";
  if (it.kind === "trait_method") return "";
  const cls = it.vis_label === "pub" ? "vis pub" : "vis";
  const title = it.vis_scope ? `visible in ${it.vis_scope}` : "public";
  return `<span class="${cls}" title="${esc(title)}">${esc(it.vis_label)}</span>`;
}

// ---------------------------------------------------------------------------
// Code rendering with cross links

const KEYWORDS = new Set("as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type union unsafe use where while macro_rules".split(" "));
const TOKEN_RE = /(\/\/[^\n]*)|("(?:[^"\\]|\\.)*")|('(?:[^'\\]|\\.)')|('[a-zA-Z_]\w*)|(#!?\[[^\]\n]*\])|(\b\d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?(?:[iuf](?:8|16|32|64|128|size))?\b)|(\.?[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)/g;

function refHtml(text, ref, extraCls = "") {
  if (ref.id && items[ref.id]) {
    const t = items[ref.id];
    return `<a class="ref ${ref.inferred ? "inferred" : ""} ${extraCls}" data-id="${esc(ref.id)}" data-kind="${t.kind}" data-path="${esc(ref.path)}">${esc(text)}</a>`;
  }
  return `<span class="ref ext ${extraCls}" data-path="${esc(ref.path)}">${esc(text)}</span>`;
}

/** Syntax-highlights Rust code and links every name found in `refs`. */
function codeHtml(text, refs = {}) {
  let out = "";
  let last = 0;
  TOKEN_RE.lastIndex = 0;
  let m;
  while ((m = TOKEN_RE.exec(text))) {
    out += esc(text.slice(last, m.index));
    last = TOKEN_RE.lastIndex;
    const [tok, comment, str, chr, life, attr, num, path] = m;
    if (comment) out += `<span class="cm">${esc(tok)}</span>`;
    else if (str || chr) out += `<span class="str">${esc(tok)}</span>`;
    else if (life) out += `<span class="kw">${esc(tok)}</span>`;
    else if (attr) out += `<span class="attr">${esc(tok)}</span>`;
    else if (num) out += `<span class="num">${esc(tok)}</span>`;
    else if (path) out += pathHtml(path, refs);
  }
  return out + esc(text.slice(last));
}

function pathHtml(path, refs) {
  if (path.startsWith(".")) {
    const r = refs[path];
    return r ? "." + refHtml(path.slice(1), r) : esc(path);
  }
  if (KEYWORDS.has(path)) return `<span class="kw">${path}</span>`;
  if (refs[path]) return refHtml(path, refs[path]);
  // Link the longest known prefix and recurse on the rest.
  const segs = path.split("::");
  for (let n = segs.length - 1; n >= 1; n--) {
    const head = segs.slice(0, n).join("::");
    if (refs[head]) return refHtml(head, refs[head]) + "::" + pathHtml(segs.slice(n).join("::"), refs);
  }
  if (segs.length > 1) return segs.map((s) => pathHtml(s, refs)).join("::");
  return esc(path);
}

/** Minimal markdown for doc comments, with intra-doc links. */
function docsHtml(md, refs = {}) {
  if (!md) return "";
  const blocks = [];
  const lines = md.split("\n");
  let para = [];
  let list = null;
  const flush = () => {
    if (para.length) blocks.push(`<p>${inlineMd(para.join(" "), refs)}</p>`);
    para = [];
    if (list) blocks.push(`<ul>${list.map((l) => `<li>${inlineMd(l, refs)}</li>`).join("")}</ul>`);
    list = null;
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*```/.test(line)) {
      flush();
      const code = [];
      for (i++; i < lines.length && !/^\s*```/.test(lines[i]); i++) {
        if (!/^\s*# /.test(lines[i])) code.push(lines[i]);
      }
      blocks.push(`<pre>${codeHtml(code.join("\n"))}</pre>`);
    } else if (/^\s*[-*] /.test(line)) {
      if (para.length) flush();
      (list ||= []).push(line.replace(/^\s*[-*] /, ""));
    } else if (/^#+ /.test(line)) {
      flush();
      blocks.push(`<p><b>${inlineMd(line.replace(/^#+ /, ""), refs)}</b></p>`);
    } else if (/^\s*\[[^\]]+\]:\s/.test(line)) {
      // Link reference definition: not shown.
    } else if (!line.trim()) {
      flush();
    } else if (list && /^\s+/.test(line)) {
      list[list.length - 1] += " " + line.trim();
    } else {
      if (list) flush();
      para.push(line.trim());
    }
  }
  flush();
  return blocks.join("");
}

function inlineMd(s, refs) {
  const parts = [];
  const re = /\[([^\]]+)\]\(([^)]+)\)|\[([^\]]+)\](?:\[([^\]]*)\])?|`([^`]+)`|\*\*([^*]+)\*\*|\*([^*]+)\*/g;
  let last = 0;
  let m;
  while ((m = re.exec(s))) {
    parts.push(esc(s.slice(last, m.index)));
    last = re.lastIndex;
    if (m[1] !== undefined) {
      const ref = refs[m[2]];
      const label = m[1].replace(/^`|`$/g, "");
      const inner = /^`/.test(m[1]) ? `<code>${esc(label)}</code>` : esc(label);
      if (ref) parts.push(ref.id ? `<a class="ref" data-id="${esc(ref.id)}" data-kind="${items[ref.id]?.kind}" data-path="${esc(ref.path)}">${inner}</a>` : `<span class="ref ext" data-path="${esc(ref.path)}">${inner}</span>`);
      else if (/^https?:/.test(m[2])) parts.push(`<a href="${esc(m[2])}" target="_blank" rel="noopener">${inner}</a>`);
      else parts.push(inner);
    } else if (m[3] !== undefined) {
      // `[x]` and reference-style `[x][y]` links.
      const ref = m[4] ? refs[m[4]] : refs[m[3]];
      const label = m[3].replace(/^`|`$/g, "");
      const inner = /^`/.test(m[3]) ? `<code>${esc(label)}</code>` : esc(label);
      if (ref) parts.push(ref.id ? `<a class="ref" data-id="${esc(ref.id)}" data-kind="${items[ref.id]?.kind}" data-path="${esc(ref.path)}">${inner}</a>` : `<span class="ref ext" data-path="${esc(ref.path)}">${inner}</span>`);
      else parts.push(inner);
    } else if (m[5] !== undefined) parts.push(`<code>${esc(m[5])}</code>`);
    else if (m[6] !== undefined) parts.push(`<b>${esc(m[6])}</b>`);
    else if (m[7] !== undefined) parts.push(`<i>${esc(m[7])}</i>`);
  }
  parts.push(esc(s.slice(last)));
  return parts.join("");
}

/** The signature to show for an item when looking from `scope`. */
function sigFor(it, scope) {
  if (it.sig_public && !within(scope, modOf(it))) return it.sig_public;
  return it.sig;
}

// ---------------------------------------------------------------------------
// State

const S = {
  view: "overview",
  module: ROOTS[0],
  scope: ROOTS[0],
  groups: [],
  links: [],
  nextGid: 1,
  pan: { x: 60, y: 30, k: 1 },
  frames: false,
  showUnchanged: false,
};

function setView(v) {
  S.view = v;
  for (const b of document.querySelectorAll("#views button")) b.classList.toggle("on", b.dataset.view === v);
  for (const sec of document.querySelectorAll(".view")) sec.classList.toggle("active", sec.id === "view-" + v);
  if (v === "overview") renderOverview();
  if (v === "module") renderModulePage();
  if (v === "canvas") renderCanvas();
  if (v === "space") Space.show();
  renderSidebar();
}

function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.add("show");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => t.classList.remove("show"), 2600);
}

// ---------------------------------------------------------------------------
// Sidebar: module tree

function renderSidebar() {
  const sb = $("#sidebar");
  const tree = (m) => {
    const it = items[m];
    const subs = moduleChildren(m).filter((c) => c.kind === "mod").sort((a, b) => a.name.localeCompare(b.name));
    const n = changesIn(m, false).length;
    const priv = !isRoot(it) && it.vis_scope;
    const sel = S.view === "module" && S.module === m;
    return `<li><div class="tree-row ${priv ? "priv" : ""} ${sel ? "sel" : ""}" data-mod="${esc(m)}" title="${esc(it.vis_label)} ${esc(m)}">
      <span class="dot ${it.status === "unchanged" ? (touched(it) ? "changed" : "unchanged") : it.status}"></span>
      <span class="nm">${esc(isRoot(it) ? it.name : it.name)}</span>${priv ? '<span class="vis">priv</span>' : ""}
      <span class="cnt">${n ? n : ""}</span></div>
      ${subs.length ? `<ul class="tree">${subs.map((s) => tree(s.id)).join("")}</ul>` : ""}</li>`;
  };
  sb.innerHTML = `<div class="side-h">Modules</div><ul class="tree">${ROOTS.map(tree).join("")}</ul>
    <div class="side-legend">
      <div><span class="dot added"></span> added &nbsp; <span class="dot changed"></span> changed &nbsp; <span class="dot removed"></span> removed</div>
      <div>Numbers: changed items defined directly in the module.</div>
    </div>
    ${D.warnings.map((w) => `<div class="side-warn">⚠ ${esc(w)}</div>`).join("")}`;
  for (const row of sb.querySelectorAll(".tree-row")) {
    row.onclick = () => {
      S.module = row.dataset.mod;
      setView("module");
    };
  }
}

// ---------------------------------------------------------------------------
// Item rows for the overview and module pages

function itemRow(it, scope, opts = {}) {
  const cls = ["irow", it.status, opts.context ? "context" : "", opts.member ? "member" : ""].join(" ");
  const sig = sigFor(it, scope);
  const firstPara = (it.docs || "").split(/\n\s*\n/)[0];
  return `<div class="${cls}" data-id="${esc(it.id)}">
    <div class="ihead"><span class="kind">${kindLabel(it.kind)}</span>${titleHtml(it, opts.withModule)} ${visHtml(it)} ${statusHtml(it)}</div>
    <div class="iacts"><button data-open="${esc(it.id)}" title="Open on the canvas">Open ▸</button></div>
    ${it.kind === "mod" ? "" : `<pre class="sig">${codeHtml(sig, it.sig_refs)}</pre>`}
    ${firstPara ? `<div class="docs">${docsHtml(firstPara, it.doc_refs)}</div>` : ""}
  </div>`;
}

/** Rows for an item plus its changed members (types with changed methods). */
function itemWithMembers(it, scope, filter, opts = {}) {
  let html = "";
  const members = it.kind === "mod" ? [] : membersOf(it).filter((m) => m.kind !== "impl" || m.parent === it.id);
  const shownMembers = [];
  for (const m of members) {
    if (m.kind === "impl") {
      // Trait impl: show it when it or its members changed.
      if (filter(m) || membersOf(m).some(filter)) shownMembers.push(m);
    } else if (filter(m)) shownMembers.push(m);
  }
  if (!filter(it) && !shownMembers.length) return "";
  html += itemRow(it, scope, { ...opts, context: !changed(it) });
  for (const m of shownMembers) html += itemRow(m, scope, { ...opts, member: true, context: !changed(m) });
  return html;
}

function bindRows(root) {
  for (const b of root.querySelectorAll("[data-open]")) {
    b.onclick = (e) => {
      e.stopPropagation();
      openOnCanvas(b.dataset.open, { switchView: true });
    };
  }
  bindRefs(root);
}

// ---------------------------------------------------------------------------
// Overview: how does the crate's public surface change?

function renderOverview() {
  const el = $("#view-overview");
  const all = Object.values(items).filter((it) => it.kind !== "mod");
  const count = (st) => all.filter((it) => it.status === st).length;
  const pubChanged = all.filter((it) => it.public && changed(it));
  const internal = all.filter((it) => !it.public && changed(it));
  const isPub = (it) => it.public && changed(it);

  const modBlock = (m) => {
    const mod = items[m];
    if (!mod.public && !isRoot(mod)) return "";
    const entries = moduleChildren(m)
      .filter((c) => c.kind !== "mod" && c.public)
      .sort(byKindName)
      .map((c) => itemWithMembers(c, "<extern>", isPub))
      .join("");
    const subs = moduleChildren(m).filter((c) => c.kind === "mod").sort(byKindName).map((c) => modBlock(c.id)).join("");
    // Public items of private modules are reachable through `pub use`.
    const reexported = !isRoot(mod) ? "" : Object.values(items)
      .filter((c) => within(modOf(c), m) && c.public && !c.parent && c.kind !== "mod" && !items[c.module].public)
      .sort(byKindName)
      .map((c) => itemWithMembers(c, "<extern>", isPub, { withModule: true }))
      .join("");
    if (!entries && !subs && !reexported) return "";
    return `<div class="modbox"><div class="modbox-h"><span class="kind">${isRoot(mod) ? "crate" : "mod"}</span>${titleHtml(mod)} ${statusHtml(mod)}
      <span class="grow" style="flex:1"></span><a data-mod="${esc(m)}">module page ›</a></div>
      <div class="modbox-b">${entries}${reexported ? `<div class="section-label">re-exported from private modules</div>${reexported}` : ""}${subs}</div></div>`;
  };
  const internalByMod = {};
  for (const it of internal) (internalByMod[modOf(it)] ||= []).push(it);

  el.innerHTML = `<h1>${esc(D.meta.title)}</h1>
    <div class="sub">${esc(D.meta.base)} → ${esc(D.meta.head)} ${D.meta.url ? `· <a href="${esc(D.meta.url)}" target="_blank" rel="noopener">view on GitHub</a>` : ""}</div>
    <div class="stats">
      <div class="stat"><b style="color:var(--added)">${count("added")}</b><span>items added</span></div>
      <div class="stat"><b style="color:var(--changed)">${count("changed")}</b><span>items changed</span></div>
      <div class="stat"><b style="color:var(--removed)">${count("removed")}</b><span>items removed</span></div>
      <div class="stat"><b>${pubChanged.length}</b><span>in the public API</span></div>
      <div class="stat"><b>${all.filter((it) => it.changes.cosmetic).length}</b><span>formatting-only (hidden)</span></div>
    </div>
    <h2>Public API changes</h2>
    <p class="sub">Items reachable from outside the crate that were added, changed or removed, in rustdoc form. Types appear as context when only their members changed.</p>
    ${ROOTS.map(modBlock).join("") || `<div class="empty-state">This pull request does not change the public API.</div>`}
    <h2>Internal changes</h2>
    <p class="sub">Private and crate-internal items are kept out of the overview. Open a module to see its private items and internal call graph.</p>
    ${Object.keys(internalByMod).sort().map((m) => `<div class="irow"><div class="ihead"><a data-mod="${esc(m)}" class="title-name"><span class="k-mod">${esc(modLabel(m))}</span></a>
      <span class="dim">${internalByMod[m].length} private item${internalByMod[m].length > 1 ? "s" : ""}: ${internalByMod[m].map((it) => esc(displayName(it))).join(", ")}</span></div>
      <div class="iacts"><button data-graph="${esc(m)}">Internal call graph ▸</button></div></div>`).join("") || `<div class="empty-state">No internal changes.</div>`}`;
  bindRows(el);
  for (const a of el.querySelectorAll("[data-mod]")) a.onclick = () => { S.module = a.dataset.mod; setView("module"); };
  for (const b of el.querySelectorAll("[data-graph]")) b.onclick = () => showModuleGraph(b.dataset.graph);
}

function byKindName(a, b) {
  const order = ["mod", "macro", "struct", "enum", "union", "trait", "type_alias", "fn", "const", "static", "impl"];
  return order.indexOf(a.kind) - order.indexOf(b.kind) || a.name.localeCompare(b.name);
}

// ---------------------------------------------------------------------------
// Module page: a module as a privacy boundary

function renderModulePage() {
  const el = $("#view-module");
  const m = S.module;
  const mod = items[m];
  const chain = modChain(m);
  const show = (it) => S.showUnchanged || touched(it);
  const filter = (it) => S.showUnchanged || changed(it);

  const section = (list, scope) => {
    const shown = list.filter(show).sort(byKindName);
    const hidden = list.length - shown.length;
    return shown.map((it) => itemWithMembers(it, scope, S.showUnchanged ? () => true : changed, {})).join("") +
      (hidden ? `<div class="collapsed-note">${hidden} unchanged item${hidden > 1 ? "s" : ""} not shown</div>` : "");
  };

  const children = moduleChildren(m).filter((c) => c.kind !== "mod");
  const exposed = children.filter((c) => !privateTo(c, m));
  const priv = children.filter((c) => privateTo(c, m));
  const subs = moduleChildren(m).filter((c) => c.kind === "mod").sort(byKindName);

  // A submodule seen from here: only what it exposes to this module.
  const subBox = (s) => {
    const kids = moduleChildren(s.id);
    const visible = kids.filter((k) => k.kind !== "mod" && visibleFrom(k.id, m));
    const hiddenChanged = changesIn(s.id).filter((it) => !visibleFrom(it.id, m)).length;
    const nested = kids.filter((k) => k.kind === "mod" && visibleFrom(k.id, m));
    const body = section(visible, m) + nested.map(subBox).join("");
    return `<div class="modbox"><div class="modbox-h"><span class="kind">mod</span>${titleHtml(s)} ${visHtml(s)} ${statusHtml(s)}
      <span style="flex:1"></span><a data-mod="${esc(s.id)}">open ›</a></div>
      <div class="modbox-b">${body || ""}
      ${hiddenChanged ? `<div class="hidden-note">${hiddenChanged} changed item${hiddenChanged > 1 ? "s are" : " is"} private to <b>${esc(s.name)}</b> and not visible from here. <a data-mod="${esc(s.id)}">Open the module</a> to see ${hiddenChanged > 1 ? "them" : "it"}.</div>` : ""}
      ${!body && !hiddenChanged ? `<div class="collapsed-note">Nothing visible from here changed.</div>` : ""}</div></div>`;
  };

  el.innerHTML = `<div class="crumbs">${chain.map((c) => `<a data-mod="${esc(c)}">${esc(items[c].name)}</a>`).join("::")}</div>
    <h1><span class="kind">${isRoot(mod) ? "crate" : "module"}</span> ${titleHtml(mod)} ${visHtml(mod)} ${statusHtml(mod)}</h1>
    <div class="sub">${esc(mod.file)} ·
      <label><input type="checkbox" id="show-unchanged" ${S.showUnchanged ? "checked" : ""}> show unchanged items</label> ·
      <button id="mod-graph">Internal call graph on canvas ▸</button></div>
    ${mod.docs ? `<div class="docs">${docsHtml(mod.docs, mod.doc_refs)}</div>` : ""}
    ${mod.changes?.docs ? `<details class="docs-old"><summary>Module docs changed — show previous</summary><div class="docs">${docsHtml(mod.old_docs || "")}</div></details>` : ""}
    <h2>Exposed by this module</h2>
    <div>${section(exposed, m) || `<div class="empty-state">No exposed items${S.showUnchanged ? "" : " changed"}.</div>`}</div>
    <h2>Private to this module</h2>
    <div class="private-zone">${section(priv, m) || `<div class="empty-state">No private items${S.showUnchanged ? "" : " changed"}.</div>`}</div>
    ${subs.length ? `<h2>Submodules</h2><p class="sub">Each submodule shows only what it makes visible to <code>${esc(shortPath(m))}</code>.</p>${subs.map(subBox).join("")}` : ""}`;
  bindRows(el);
  for (const a of el.querySelectorAll("[data-mod]")) a.onclick = () => { S.module = a.dataset.mod; setView("module"); };
  $("#show-unchanged").onchange = (e) => { S.showUnchanged = e.target.checked; renderModulePage(); };
  $("#mod-graph").onclick = () => showModuleGraph(m);
}

/** Opens a module's changed items and their direct neighbours. */
function showModuleGraph(m) {
  S.scope = m;
  const own = changesIn(m, false).filter((it) => it.kind !== "impl");
  if (!own.length) {
    toast(`No changed items directly in ${modLabel(m)}`);
  }
  S.groups = [];
  S.links = [];
  const ids = new Set(own.map((it) => it.id));
  // Unchanged neighbours inside the module, as context for the changes.
  for (const it of own) {
    for (const n of [...it.calls.map((c) => c.to), ...it.callers.map((c) => c.from)]) {
      if (items[n] && modOf(items[n]) === m && items[n].kind !== "impl") ids.add(n);
    }
  }
  for (const id of ids) addGroup(id, 0, 0);
  setView("canvas");
  arrangeByModule();
  renderCanvas();
  fitView();
}

// ---------------------------------------------------------------------------
// Canvas

const CARD_W = 440;

function groupOf(id) {
  return S.groups.find((g) => g.tabs.includes(id));
}

function addGroup(id, x, y) {
  const g = { gid: S.nextGid++, tabs: [id], active: id, x, y, openSrc: new Set() };
  S.groups.push(g);
  return g;
}

function groupVisible(g) {
  return visibleFrom(g.active, S.scope);
}

/**
 * Places `id` on the canvas. With `from`, the card goes next to that card
 * and a connector records the link.
 */
function openOnCanvas(id, opts = {}) {
  const it = items[id];
  if (!it) return;
  if (!visibleFrom(id, S.scope)) {
    S.scope = modOf(it);
    toast(`Drilled into ${modLabel(S.scope)} — ${displayName(it)} is private there`);
  }
  let g = groupOf(id);
  if (g) {
    g.active = id;
  } else {
    const from = opts.from && groupOf(opts.from);
    const side = opts.side ?? 1;
    let x, y;
    if (from) {
      const el = cardEl(from);
      x = from.x + (side > 0 ? CARD_W + 80 : -(CARD_W + 80));
      y = from.y + (opts.offset ?? 0);
      if (el && side === 0) { x = from.x; y = from.y + el.offsetHeight + 40; }
    } else {
      const vp = $("#viewport").getBoundingClientRect();
      x = (vp.width / 2 - S.pan.x) / S.pan.k - CARD_W / 2;
      y = (80 - S.pan.y) / S.pan.k;
    }
    g = addGroup(id, x, y);
    const p = freeSpot(g, x, y);
    g.x = p.x;
    g.y = p.y;
    if (opts.from && opts.from !== id) S.links.push({ from: opts.from, to: id });
  }
  if (opts.switchView && S.view !== "canvas") setView("canvas");
  else renderCanvas();
  flash(g);
  if (opts.switchView) ensureVisible(g);
}

function cardEl(g) {
  return document.querySelector(`.card[data-gid="${g.gid}"]`);
}

function estimateHeight(g) {
  const el = cardEl(g);
  if (el && el.offsetHeight) return el.offsetHeight;
  const it = items[g.active];
  return 120 + (it.sig.split("\n").length * 18) + (it.docs ? 40 : 0);
}

function freeSpot(g, x, y) {
  const h = estimateHeight(g);
  const others = S.groups.filter((o) => o !== g && groupVisible(o));
  for (let i = 0; i < 80; i++) {
    const hit = others.find((o) => {
      const oh = estimateHeight(o);
      return x < o.x + CARD_W + 20 && x + CARD_W + 20 > o.x && y < o.y + oh + 20 && y + h + 20 > o.y;
    });
    if (!hit) return { x, y };
    y = hit.y + estimateHeight(hit) + 30;
  }
  return { x, y };
}

function flash(g) {
  requestAnimationFrame(() => {
    const el = cardEl(g);
    if (!el) return;
    el.classList.remove("flash");
    void el.offsetWidth;
    el.classList.add("flash");
  });
}

function ensureVisible(g) {
  requestAnimationFrame(() => {
    const vp = $("#viewport").getBoundingClientRect();
    const sx = g.x * S.pan.k + S.pan.x;
    const sy = g.y * S.pan.k + S.pan.y;
    if (sx < 0 || sy < 0 || sx > vp.width - 100 || sy > vp.height - 100) {
      S.pan.x = vp.width / 2 - (g.x + CARD_W / 2) * S.pan.k;
      S.pan.y = 60 - g.y * S.pan.k;
      applyPan();
      drawEdges();
    }
  });
}

function closeTab(g, id) {
  g.tabs = g.tabs.filter((t) => t !== id);
  S.links = S.links.filter((l) => l.from !== id && l.to !== id);
  if (!g.tabs.length) S.groups = S.groups.filter((o) => o !== g);
  else if (g.active === id) g.active = g.tabs[0];
  renderCanvas();
}

/** Opens a list of related items next to a card. */
function expand(id, kind) {
  const it = items[id];
  let targets = [];
  let side = 1;
  if (kind === "callees") targets = it.calls.map((c) => c.to);
  if (kind === "callers") { targets = it.callers.map((c) => c.from); side = -1; }
  if (kind === "uses") { targets = it.usedBy.map((u) => u.from); side = -1; }
  if (kind === "members") targets = membersOf(it).map((m) => m.id);
  targets = [...new Set(targets)].filter((t) => items[t]);
  const visible = targets.filter((t) => visibleFrom(t, S.scope));
  const hidden = targets.length - visible.length;
  if (!visible.length) {
    toast(hidden ? `All ${hidden} are private to submodules of ${modLabel(S.scope)}` : `Nothing to show`);
    return;
  }
  // Changed items first; unchanged ones are only context.
  visible.sort((a, b) => (changed(items[b]) - changed(items[a])) || a.localeCompare(b));
  const LIMIT = 10;
  const fresh = visible.filter((t) => !groupOf(t));
  const skipped = fresh.slice(LIMIT);
  const from = groupOf(id);
  let offset = 0;
  for (const t of visible.filter((t) => !skipped.includes(t))) {
    if (groupOf(t)) {
      if (!S.links.some((l) => (l.from === id && l.to === t) || (l.from === t && l.to === id))) S.links.push({ from: id, to: t });
      continue;
    }
    const g = addGroup(t, from.x + side * (CARD_W + 90), from.y + offset);
    const p = freeSpot(g, g.x, g.y);
    g.x = p.x;
    g.y = p.y;
    offset = g.y - from.y + estimateHeight(g) + 30;
    S.links.push(side > 0 ? { from: id, to: t } : { from: t, to: id });
    renderCanvas();
  }
  renderCanvas();
  const notes = [];
  if (skipped.length) notes.push(`${skipped.length} more not opened (use Find item to open them)`);
  if (hidden) notes.push(`${hidden} ${hidden > 1 ? "are" : "is"} private to submodules and hidden at this scope`);
  if (notes.length) toast(notes.join(" · "));
}

/** Opens all members of a type as tabs in one card. */
function openMembersAsTabs(id) {
  const it = items[id];
  const members = membersOf(it).filter((m) => visibleFrom(m.id, S.scope) && m.kind !== "impl");
  if (!members.length) return;
  const from = groupOf(id);
  let g = members.map((m) => groupOf(m.id)).find(Boolean);
  if (!g) {
    g = addGroup(members[0].id, from.x, from.y + estimateHeight(from) + 40);
    const p = freeSpot(g, g.x, g.y);
    g.x = p.x;
    g.y = p.y;
    S.links.push({ from: id, to: members[0].id });
  }
  for (const m of members) {
    const og = groupOf(m.id);
    if (og && og !== g) closeTab(og, m.id);
    if (!g.tabs.includes(m.id)) g.tabs.push(m.id);
  }
  g.active = members.find((m) => changed(m))?.id ?? g.tabs[0];
  renderCanvas();
}

function hiddenNote(list, label) {
  const hidden = list.filter((t) => items[t] && !visibleFrom(t, S.scope));
  if (!hidden.length) return "";
  const mods = [...new Set(hidden.map((t) => hidingModule(t, S.scope)).filter(Boolean))];
  return `<div class="hidden-note">${hidden.length} ${label} private to ${mods.map((m) => `<a data-drill="${esc(m)}">${esc(modLabel(m))}</a>`).join(", ")} — not visible from ${esc(modLabel(S.scope))}</div>`;
}

function cardHtml(g) {
  const it = items[g.active];
  const scope = S.scope;
  const tabs = g.tabs.length > 1
    ? `<div class="card-tabs">${g.tabs.map((t) => `<div class="card-tab ${t === g.active ? "active" : ""}" data-tab="${esc(t)}"><span class="dot ${items[t].status}"></span>${esc(displayName(items[t]))}<span class="x" data-close="${esc(t)}" title="Close tab">×</span></div>`).join("")}</div>`
    : "";
  const callees = [...new Set(it.calls.map((c) => c.to))];
  const callers = [...new Set(it.callers.map((c) => c.from))];
  const usedBy = [...new Set(it.usedBy.map((u) => u.from))];
  const acts = [];
  if (it.kind !== "mod") {
    if (callees.length) acts.push(`<button data-act="callees" title="Show what this calls">calls ▸ <span class="n">${callees.length}</span></button>`);
    if (callers.length) acts.push(`<button data-act="callers" title="Show what calls this">◂ callers <span class="n">${callers.length}</span></button>`);
    if (usedBy.length) {
      const ctxs = {};
      for (const u of it.usedBy) ctxs[u.ctx] = (ctxs[u.ctx] || 0) + 1;
      const where = Object.entries(ctxs).map(([c, n]) => `${n} in ${c === "body" ? "bodies" : c === "impl" ? "impls" : c + "s"}`).join(", ");
      acts.push(`<button data-act="uses" title="Show where this is used: ${esc(where)}">◂ used by <span class="n">${usedBy.length}</span> <span class="n">(${esc(where)})</span></button>`);
    }
    if (it.children.length && it.kind !== "mod") acts.push(`<button data-act="tabs" title="Open members as tabs">members as tabs</button>`);
  } else {
    acts.push(`<button data-act="drill">Drill in</button><button data-act="page">Module page</button>`);
  }
  if (g.tabs.length > 1) acts.push(`<button data-act="detach" title="Move this tab into its own card">detach tab</button>`);

  const sig = it.kind === "mod" ? "" : `<pre class="sig">${codeHtml(sigFor(it, scope), it.sig_refs)}</pre>`;
  const oldSig = it.changes.signature && it.old_sig
    ? `<div class="sig-old">previous signature:<pre class="sig">${codeHtml(it.old_sig)}</pre></div>`
    : it.changes.signature ? `<div class="sig-old">Same text, but names in the signature now refer to different items.</div>` : "";
  const docs = it.docs ? `<div class="docs">${docsHtml(it.docs, it.doc_refs)}</div>` : `<div class="docs empty">No documentation.</div>`;
  const oldDocs = it.changes.docs && it.old_docs !== undefined && it.status === "changed"
    ? `<details class="docs-old"><summary>docs changed — show previous</summary><div class="docs">${docsHtml(it.old_docs) || "<i>none</i>"}</div></details>` : "";

  // Members listing like a rustdoc type page.
  let members = "";
  const ms = it.kind === "mod" ? moduleChildren(it.id) : membersOf(it);
  if (ms.length) {
    const vis = ms.filter((m) => visibleFrom(m.id, scope));
    const groups = {};
    for (const m of vis) {
      const label = m.kind === "impl" ? (it.kind === "trait" ? "Implementors" : "Trait implementations") : it.kind === "mod" ? kindLabel(m.kind) + "s" : "Methods & items";
      (groups[label] ||= []).push(m);
    }
    members = `<div class="members">${Object.entries(groups).map(([label, list]) => `<div class="mgroup"><div class="mlabel">${label}</div>${list.sort(byKindName).map((m) => `<div class="mitem ${m.vis_scope ? "private" : ""}"><span class="dot ${m.status === "unchanged" && touched(m) ? "changed" : m.status}"></span><a class="ref" data-id="${esc(m.id)}" data-kind="${m.kind}" data-path="${esc(m.id)}">${esc(m.kind === "impl" ? m.name : m.name)}</a>${m.vis_scope ? `<span class="vis">${esc(m.vis_label)}</span>` : ""}</div>`).join("")}</div>`).join("")}
      ${ms.length > vis.length ? `<div class="hidden-note">${ms.length - vis.length} more not visible from ${esc(modLabel(scope))}</div>` : ""}</div>`;
  }

  const src = it.source.length
    ? (() => {
        const add = it.source.filter((l) => l[0] === "+").length;
        const del = it.source.filter((l) => l[0] === "-").length;
        const refs = { ...it.sig_refs, ...it.body_refs };
        const lines = it.source.map(([t, s]) => `<span class="ln ${t === "+" ? "add" : t === "-" ? "del" : ""}" data-t="${t === " " ? "" : t}">${codeHtml(s, refs) || " "}</span>`).join("");
        const diffInfo = add || del ? ` · <span style="color:var(--added)">+${add}</span> <span style="color:var(--removed)">−${del}</span>` : "";
        return `<details class="src" data-src="${esc(it.id)}" ${g.openSrc.has(it.id) ? "open" : ""}><summary>Source · ${esc(it.file)}:${it.line} · ${it.source.length} lines${diffInfo}</summary><pre>${lines}</pre></details>`;
      })()
    : "";

  const impl = it.impl_header ? `<div class="card-impl">${codeHtml(it.impl_header)}</div>` : "";
  const context = !changed(it) && !touched(it) ? "context" : "";
  return `<div class="card cs-${it.status} ${context}" data-gid="${g.gid}" style="left:${g.x}px;top:${g.y}px">
    ${tabs}
    <div class="card-h"><span class="kind">${kindLabel(it.kind)}</span>${titleHtml(it)} ${visHtml(it)}<span class="grow"></span>${statusHtml(it)}<span class="x" data-close="${esc(it.id)}" title="Close">×</span></div>
    <div class="card-loc">${esc(modLabel(modOf(it)))}</div>
    <div class="card-b">${impl}${sig}${oldSig}${docs}${oldDocs}${members}
      ${hiddenNote(callees, "callees are")}${hiddenNote(callers, "callers are")}
      <div class="card-acts">${acts.join("")}</div>${src}</div>
  </div>`;
}

function renderCanvas() {
  const bar = $("#canvas-bar");
  const chain = modChain(S.scope);
  const hiddenCards = S.groups.filter((g) => !groupVisible(g)).length;
  bar.innerHTML = `<span>Viewing from</span><span class="scope">${chain.map((c) => `<a data-scope="${esc(c)}" title="Look from ${esc(modLabel(c))}">${esc(items[c].name)}</a>`).join("::")}</span>
    ${hiddenCards ? `<span class="chip" title="Private to submodules of the current scope">${hiddenCards} card${hiddenCards > 1 ? "s" : ""} hidden at this scope</span>` : ""}
    <span class="sep"></span>
    <button id="cv-arrange" title="Lay cards out inside their modules">Arrange by module</button>
    <button id="cv-frames" class="${S.frames ? "on" : ""}" title="Draw module boundaries">Module frames</button>
    <button id="cv-fit">Fit</button>
    <button id="cv-clear">Clear</button>`;
  for (const a of bar.querySelectorAll("[data-scope]")) a.onclick = () => { S.scope = a.dataset.scope; renderCanvas(); };
  $("#cv-arrange").onclick = () => { arrangeByModule(); renderCanvas(); fitView(); };
  $("#cv-frames").onclick = () => { S.frames = !S.frames; renderCanvas(); };
  $("#cv-fit").onclick = fitView;
  $("#cv-clear").onclick = () => { S.groups = []; S.links = []; renderCanvas(); };

  const cards = $("#cards");
  cards.innerHTML = S.groups.filter(groupVisible).map(cardHtml).join("");
  $("#canvas-empty").innerHTML = S.groups.some(groupVisible) ? "" :
    `<div>Nothing on the canvas yet.<br>Open items from the <b>Overview</b> or a <b>module page</b>, or use <b>Find item</b>.<br>Click any name in a card to place its item next to it.</div>`;
  for (const el of cards.querySelectorAll(".card")) bindCard(el);
  applyPan();
  requestAnimationFrame(() => { drawEdges(); markOnCanvas(); });
  markOnCanvas();
}

function bindCard(el) {
  const g = S.groups.find((o) => o.gid === +el.dataset.gid);
  const id = g.active;
  for (const t of el.querySelectorAll("[data-tab]")) t.onclick = (e) => { if (e.target.dataset.close) return; g.active = t.dataset.tab; renderCanvas(); };
  for (const x of el.querySelectorAll("[data-close]")) x.onclick = (e) => { e.stopPropagation(); closeTab(g, x.dataset.close); };
  for (const b of el.querySelectorAll("[data-act]")) {
    b.onclick = () => {
      const a = b.dataset.act;
      if (a === "callees" || a === "callers" || a === "uses") expand(id, a);
      if (a === "tabs") openMembersAsTabs(id);
      if (a === "drill") { S.scope = id; renderCanvas(); }
      if (a === "page") { S.module = id; setView("module"); }
      if (a === "detach") {
        g.tabs = g.tabs.filter((t) => t !== id);
        g.active = g.tabs[0];
        const ng = addGroup(id, g.x + CARD_W + 60, g.y);
        const p = freeSpot(ng, ng.x, ng.y);
        ng.x = p.x; ng.y = p.y;
        renderCanvas();
      }
    };
  }
  for (const a of el.querySelectorAll("[data-drill]")) a.onclick = () => { S.scope = a.dataset.drill; renderCanvas(); toast(`Now viewing from ${modLabel(S.scope)}`); };
  const det = el.querySelector("details.src");
  if (det) det.ontoggle = () => { det.open ? g.openSrc.add(det.dataset.src) : g.openSrc.delete(det.dataset.src); drawEdges(); };
  bindRefs(el, id);
  // Dragging by the header (or tab bar); dropping onto another card's header merges into tabs.
  const handles = [el.querySelector(".card-h"), el.querySelector(".card-tabs")].filter(Boolean);
  for (const h of handles) {
    h.onpointerdown = (e) => {
      if (e.target.closest("[data-close], button, a")) return;
      e.preventDefault();
      const start = { x: e.clientX, y: e.clientY, gx: g.x, gy: g.y };
      el.style.zIndex = 10;
      el.style.pointerEvents = "none";
      const move = (ev) => {
        g.x = start.gx + (ev.clientX - start.x) / S.pan.k;
        g.y = start.gy + (ev.clientY - start.y) / S.pan.k;
        el.style.left = g.x + "px";
        el.style.top = g.y + "px";
        drawEdges();
        const under = document.elementFromPoint(ev.clientX, ev.clientY)?.closest(".card");
        document.querySelectorAll(".card.hover-target").forEach((c) => c.classList.remove("hover-target"));
        if (under && under !== el && ev.clientY - under.getBoundingClientRect().top < 60) under.classList.add("hover-target");
      };
      const up = (ev) => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        const under = document.elementFromPoint(ev.clientX, ev.clientY)?.closest(".card");
        el.style.zIndex = "";
        el.style.pointerEvents = "";
        document.querySelectorAll(".card.hover-target").forEach((c) => c.classList.remove("hover-target"));
        if (under && under !== el && ev.clientY - under.getBoundingClientRect().top < 60) {
          const target = S.groups.find((o) => o.gid === +under.dataset.gid);
          target.tabs.push(...g.tabs.filter((t) => !target.tabs.includes(t)));
          target.active = g.active;
          S.groups = S.groups.filter((o) => o !== g);
          toast("Merged into tabs");
        }
        renderCanvas();
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
    };
  }
}

/** Clicking a name places its item next to `fromId` (or opens it). */
function bindRefs(root, fromId) {
  for (const a of root.querySelectorAll("a.ref[data-id]")) {
    a.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      const target = a.dataset.id;
      if (fromId) openOnCanvas(target, { from: fromId });
      else openOnCanvas(target, { switchView: true });
    };
  }
}

/** Highlights names that refer to items already on the canvas. */
function markOnCanvas() {
  const on = new Set(S.groups.filter(groupVisible).map((g) => g.active));
  for (const a of document.querySelectorAll("#cards a.ref[data-id]")) {
    const own = a.closest(".card");
    const ownId = own && S.groups.find((g) => g.gid === +own.dataset.gid)?.active;
    a.classList.toggle("on-canvas", on.has(a.dataset.id) && a.dataset.id !== ownId);
  }
}

function applyPan() {
  $("#world").style.transform = `translate(${S.pan.x}px, ${S.pan.y}px) scale(${S.pan.k})`;
}

function cardRect(g) {
  const el = cardEl(g);
  return el ? { x: g.x, y: g.y, w: el.offsetWidth, h: el.offsetHeight } : null;
}

function edgePath(a, b) {
  const ac = { x: a.x + a.w / 2, y: a.y + a.h / 2 };
  const bc = { x: b.x + b.w / 2, y: b.y + b.h / 2 };
  let p1, p2, c1, c2;
  if (Math.abs(bc.x - ac.x) > (a.w + b.w) / 2 - 40) {
    const dir = bc.x > ac.x ? 1 : -1;
    p1 = { x: dir > 0 ? a.x + a.w : a.x, y: Math.min(a.y + 40, ac.y) };
    p2 = { x: dir > 0 ? b.x : b.x + b.w, y: Math.min(b.y + 40, bc.y) };
    const dx = Math.max(40, Math.abs(p2.x - p1.x) / 2);
    c1 = { x: p1.x + dir * dx, y: p1.y };
    c2 = { x: p2.x - dir * dx, y: p2.y };
  } else {
    const dir = bc.y > ac.y ? 1 : -1;
    p1 = { x: ac.x, y: dir > 0 ? a.y + a.h : a.y };
    p2 = { x: bc.x, y: dir > 0 ? b.y : b.y + b.h };
    const dy = Math.max(30, Math.abs(p2.y - p1.y) / 2);
    c1 = { x: p1.x, y: p1.y + dir * dy };
    c2 = { x: p2.x, y: p2.y - dir * dy };
  }
  return { d: `M${p1.x},${p1.y} C${c1.x},${c1.y} ${c2.x},${c2.y} ${p2.x},${p2.y}`, end: p2 };
}

/** Draws calls, uses, member and link connectors between visible cards. */
function drawEdges() {
  const svg = $("#edges");
  const vis = S.groups.filter(groupVisible);
  const rects = new Map(vis.map((g) => [g.gid, cardRect(g)]));
  const out = [];
  const seen = new Set();
  const add = (ga, gb, cls, title) => {
    if (ga === gb) return;
    const key = ga.gid + ">" + gb.gid;
    if (seen.has(key)) return;
    seen.add(key);
    const a = rects.get(ga.gid);
    const b = rects.get(gb.gid);
    if (!a || !b) return;
    const { d } = edgePath(a, b);
    const arrow = cls.includes("member") || cls.includes("link") ? "" : `marker-end="url(#arrow-${cls.split(" ")[0]})"`;
    out.push(`<path class="edge ${cls}" d="${d}" ${arrow}><title>${esc(title)}</title></path>`);
  };
  for (const ga of vis) {
    const a = items[ga.active];
    for (const gb of vis) {
      const b = items[gb.active];
      if (a.calls.some((c) => c.to === b.id)) add(ga, gb, "call" + (a.calls.find((c) => c.to === b.id).inferred ? " inferred" : ""), `${displayName(a)} calls ${displayName(b)}`);
    }
  }
  for (const ga of vis) {
    const a = items[ga.active];
    for (const gb of vis) {
      const b = items[gb.active];
      const u = a.uses.find((x) => x.to === b.id);
      if (u) add(ga, gb, "use", `${displayName(a)} uses ${displayName(b)} (${u.ctx})`);
    }
  }
  for (const ga of vis) {
    const a = items[ga.active];
    for (const gb of vis) {
      if (items[gb.active].parent === a.id) add(ga, gb, "member", `${displayName(items[gb.active])} belongs to ${displayName(a)}`);
    }
  }
  for (const l of S.links) {
    const ga = groupOf(l.from);
    const gb = groupOf(l.to);
    if (ga && gb && groupVisible(ga) && groupVisible(gb) && !seen.has(ga.gid + ">" + gb.gid) && !seen.has(gb.gid + ">" + ga.gid)) add(ga, gb, "link", "opened from here");
  }
  svg.innerHTML = `<defs>
    <marker id="arrow-call" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="var(--fn)"/></marker>
    <marker id="arrow-use" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="var(--type)"/></marker>
    </defs>${out.join("")}`;
  drawFrames(rects);
}

/** Module boundaries around the cards they contain, with a private zone. */
function drawFrames(rects) {
  const svg = $("#frames");
  if (!S.frames) { svg.innerHTML = ""; return; }
  const vis = S.groups.filter(groupVisible);
  const byMod = new Map();
  for (const g of vis) {
    const m = modOf(items[g.active]);
    for (const c of modChain(m)) if (!byMod.has(c)) byMod.set(c, { pub: [], priv: [], frames: [] });
    const r = rects.get(g.gid);
    if (!r) continue;
    const it = items[g.active];
    (privateTo(it, m) ? byMod.get(m).priv : byMod.get(m).pub).push(r);
  }
  const union = (rs, pad) => {
    if (!rs.length) return null;
    const x1 = Math.min(...rs.map((r) => r.x)) - pad;
    const y1 = Math.min(...rs.map((r) => r.y)) - pad;
    const x2 = Math.max(...rs.map((r) => r.x + r.w)) + pad;
    const y2 = Math.max(...rs.map((r) => r.y + r.h)) + pad;
    return { x: x1, y: y1, w: x2 - x1, h: y2 - y1 };
  };
  const mods = [...byMod.keys()].sort((a, b) => modDepth(b) - modDepth(a));
  const out = [];
  const frameOf = new Map();
  for (const m of mods) {
    const e = byMod.get(m);
    const privBox = union(e.priv, 14);
    const inner = [...e.pub, ...(privBox ? [privBox] : []), ...e.frames];
    const box = union(inner, 22);
    if (!box) continue;
    box.y -= privBox && !e.pub.length ? 26 : 12;
    box.h += privBox && !e.pub.length ? 26 : 12;
    frameOf.set(m, box);
    const parent = items[m] && !isRoot(items[m]) ? items[m].module : null;
    if (parent && byMod.has(parent)) byMod.get(parent).frames.push(box);
    const it = items[m];
    let s = `<g class="frame"><rect class="frame-bg" x="${box.x}" y="${box.y}" width="${box.w}" height="${box.h}" rx="12"/>`;
    s += `<rect x="${box.x}" y="${box.y}" width="${box.w}" height="${box.h}" rx="12"/>`;
    s += `<text x="${box.x + 12}" y="${box.y + 20}">${esc(modLabel(m))}${it.vis_scope ? "  (" + esc(it.vis_label) + ")" : ""}</text>`;
    if (privBox) {
      s += `<rect class="priv" x="${privBox.x}" y="${privBox.y}" width="${privBox.w}" height="${privBox.h}" rx="8"/>`;
      s += `<text class="priv" x="${privBox.x + 8}" y="${privBox.y - 4}">private to ${esc(it.name)}</text>`;
    }
    out.push(s + "</g>");
  }
  // Outer frames first so inner ones are drawn on top.
  svg.innerHTML = out.reverse().join("");
}

/**
 * Lays out the visible cards inside nested module regions: exposed items
 * on top, private items below them, submodules underneath.
 */
function arrangeByModule() {
  S.frames = true;
  renderCanvas();
  const vis = S.groups.filter(groupVisible);
  const height = (g) => estimateHeight(g);
  const byMod = new Map();
  for (const g of vis) {
    const m = modOf(items[g.active]);
    (byMod.get(m) || byMod.set(m, []).get(m)).push(g);
  }
  const hasCards = (m) => [...byMod.keys()].some((k) => within(k, m));
  const COLS = 2;
  const GAP = 40;
  const layout = (m, x, y) => {
    let cy = y + 36;
    let w = 0;
    const own = byMod.get(m) || [];
    const placeRow = (list) => {
      list.sort((a, b) => (changed(items[b.active]) - changed(items[a.active])) || displayName(items[a.active]).localeCompare(displayName(items[b.active])));
      for (let i = 0; i < list.length; i += COLS) {
        const row = list.slice(i, i + COLS);
        let rh = 0;
        row.forEach((g, j) => {
          g.x = x + 24 + j * (CARD_W + GAP);
          g.y = cy;
          rh = Math.max(rh, height(g));
        });
        w = Math.max(w, row.length * (CARD_W + GAP));
        cy += rh + GAP;
      }
    };
    placeRow(own.filter((g) => !privateTo(items[g.active], m)));
    const priv = own.filter((g) => privateTo(items[g.active], m));
    if (priv.length) { cy += 20; placeRow(priv); }
    const subs = moduleChildren(m).filter((c) => c.kind === "mod" && hasCards(c.id)).map((c) => c.id);
    let sx = x + 24;
    let maxH = 0;
    for (const s of subs) {
      const r = layout(s, sx, cy);
      sx += r.w + GAP;
      maxH = Math.max(maxH, r.h);
    }
    w = Math.max(w, sx - x);
    cy += maxH + (subs.length ? GAP : 0);
    return { w: w + 24, h: cy - y };
  };
  let x = 0;
  for (const root of ROOTS) {
    if (!hasCards(root)) continue;
    // Start from the outermost module that is still visible at this scope.
    const r = layout(root, x, 0);
    x += r.w + 80;
  }
}

function fitView() {
  requestAnimationFrame(() => {
    const vis = S.groups.filter(groupVisible);
    if (!vis.length) return;
    const rs = vis.map(cardRect).filter(Boolean);
    const x1 = Math.min(...rs.map((r) => r.x)) - 60;
    const y1 = Math.min(...rs.map((r) => r.y)) - (S.frames ? 110 : 60);
    const x2 = Math.max(...rs.map((r) => r.x + r.w)) + 60;
    const y2 = Math.max(...rs.map((r) => r.y + r.h)) + 60;
    const vp = $("#viewport").getBoundingClientRect();
    const k = Math.min(1, vp.width / (x2 - x1), vp.height / (y2 - y1));
    S.pan = { k: Math.max(0.25, k), x: -x1 * Math.max(0.25, k) + Math.max(0, (vp.width - (x2 - x1) * k) / 2), y: -y1 * Math.max(0.25, k) };
    applyPan();
    drawEdges();
  });
}

// Panning and zooming the canvas.
(() => {
  const vp = $("#viewport");
  vp.addEventListener("pointerdown", (e) => {
    if (e.target.closest(".card")) return;
    vp.classList.add("panning");
    const start = { x: e.clientX, y: e.clientY, px: S.pan.x, py: S.pan.y };
    const move = (ev) => { S.pan.x = start.px + ev.clientX - start.x; S.pan.y = start.py + ev.clientY - start.y; applyPan(); };
    const up = () => { vp.classList.remove("panning"); window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  });
  vp.addEventListener("wheel", (e) => {
    if (e.target.closest(".card") && !e.ctrlKey) return;
    e.preventDefault();
    const r = vp.getBoundingClientRect();
    const mx = e.clientX - r.left;
    const my = e.clientY - r.top;
    const k = Math.min(2, Math.max(0.2, S.pan.k * Math.exp(-e.deltaY * 0.0015)));
    S.pan.x = mx - ((mx - S.pan.x) * k) / S.pan.k;
    S.pan.y = my - ((my - S.pan.y) * k) / S.pan.k;
    S.pan.k = k;
    applyPan();
  }, { passive: false });
})();

// ---------------------------------------------------------------------------
// Tooltip: fully qualified path of any name

(() => {
  const tip = $("#tooltip");
  document.addEventListener("mouseover", (e) => {
    const r = e.target.closest(".ref");
    document.querySelectorAll(".card.hover-target").forEach((c) => { if (!c.dataset.dragging) c.classList.remove("hover-target"); });
    if (!r) { tip.style.display = "none"; return; }
    const it = r.dataset.id && items[r.dataset.id];
    let html = `<div class="tp">${esc(r.dataset.path || r.dataset.id || "")}</div>`;
    if (it) {
      html += `<div>${kindLabel(it.kind)} · ${statusHtml(it)} ${r.classList.contains("inferred") ? " · <i>guessed from the method name</i>" : ""}</div>`;
      if (it.kind !== "mod") html += `<pre>${esc(it.sig.split("\n").slice(0, 6).join("\n"))}</pre>`;
      const g = groupOf(it.id);
      if (g && groupVisible(g)) cardEl(g)?.classList.add("hover-target");
    } else {
      html += `<div class="dim">outside the analyzed crates</div>`;
    }
    tip.innerHTML = html;
    tip.style.display = "block";
  });
  document.addEventListener("mousemove", (e) => {
    if (tip.style.display !== "block") return;
    const w = tip.offsetWidth;
    tip.style.left = Math.min(window.innerWidth - w - 8, e.clientX + 14) + "px";
    tip.style.top = e.clientY + 16 + "px";
  });
})();

// ---------------------------------------------------------------------------
// Search

(() => {
  const input = $("#search");
  const box = $("#search-results");
  const all = Object.values(items);
  let sel = 0;
  let results = [];
  const render = () => {
    const q = input.value.trim().toLowerCase();
    if (!q) { box.classList.remove("open"); return; }
    results = all.filter((it) => it.id.toLowerCase().includes(q) || displayName(it).toLowerCase().includes(q))
      .sort((a, b) => (changed(b) - changed(a)) || a.id.length - b.id.length).slice(0, 30);
    sel = Math.min(sel, results.length - 1);
    box.innerHTML = results.map((it, i) => `<div class="${i === sel ? "sel" : ""}" data-i="${i}"><span class="dot ${it.status}"></span> <span class="kind">${kindLabel(it.kind)}</span> ${esc(shortPath(it.id))}</div>`).join("") || `<div class="dim">No match</div>`;
    box.classList.add("open");
    for (const d of box.querySelectorAll("[data-i]")) d.onmousedown = () => pick(+d.dataset.i);
  };
  const pick = (i) => {
    const it = results[i];
    if (!it) return;
    input.value = "";
    box.classList.remove("open");
    if (it.kind === "mod") { S.module = it.id; setView("module"); }
    else openOnCanvas(it.id, { switchView: true });
  };
  input.oninput = () => { sel = 0; render(); };
  input.onkeydown = (e) => {
    if (e.key === "ArrowDown") { sel = Math.min(results.length - 1, sel + 1); render(); e.preventDefault(); }
    if (e.key === "ArrowUp") { sel = Math.max(0, sel - 1); render(); e.preventDefault(); }
    if (e.key === "Enter") pick(sel);
    if (e.key === "Escape") { input.value = ""; render(); }
  };
  input.onblur = () => setTimeout(() => box.classList.remove("open"), 150);
})();

// ---------------------------------------------------------------------------
// 3D view: each module is a plane holding its internal call graph

const Space = (() => {
  const canvas = $("#space");
  const ctx = canvas.getContext("2d");
  const cam = { yaw: -0.45, pitch: 0.42, dist: 1800, zoom: 1 };
  let showUses = false;
  let model = null;
  let hover = null;
  let focus = null;
  let showUnchanged = false;
  let projected = [];

  function build() {
    // Nodes: changed items plus their direct neighbours as context.
    const nodes = new Map();
    const want = (it, ctxNode) => {
      if (!it || it.kind === "mod" || it.kind === "impl") return;
      if (!nodes.has(it.id)) nodes.set(it.id, { it, context: ctxNode });
      else if (!ctxNode) nodes.get(it.id).context = false;
    };
    for (const it of Object.values(items)) {
      if (it.kind === "mod" || it.kind === "impl") continue;
      if (changed(it) || showUnchanged) {
        want(it, false);
        for (const c of it.calls) want(items[c.to], true);
        for (const c of it.callers) want(items[c.from], true);
        if (showUses) for (const u of it.uses) want(items[u.to], true);
      }
    }
    const planeIds = new Set();
    for (const n of nodes.values()) for (const m of modChain(modOf(n.it))) planeIds.add(m);
    const planes = [...planeIds].map((m) => ({ id: m, depth: modDepth(m), nodes: [] }));
    const planeOf = new Map(planes.map((p) => [p.id, p]));
    for (const n of nodes.values()) planeOf.get(modOf(n.it)).nodes.push(n);
    // Lay out planes: one row per depth, siblings side by side.
    const byDepth = {};
    for (const p of planes) (byDepth[p.depth] ||= []).push(p);
    const SP = 85;
    for (const p of planes) {
      const pub = p.nodes.filter((n) => !privateTo(n.it, p.id));
      const priv = p.nodes.filter((n) => privateTo(n.it, p.id));
      const cols = Math.max(2, Math.ceil(Math.sqrt(p.nodes.length + 1)));
      p.w = Math.max(260, cols * 170 + 60);
      const rowsPub = Math.ceil(pub.length / cols);
      const rowsPriv = Math.ceil(priv.length / cols);
      p.d = Math.max(160, (rowsPub + rowsPriv) * SP + 90);
      const place = (list, z0) => list.sort((a, b) => a.it.name.localeCompare(b.it.name)).forEach((n, i) => {
        n.lx = -p.w / 2 + 50 + (i % cols) * 170 + (Math.floor(i / cols) % 2) * 40;
        n.lz = z0 - Math.floor(i / cols) * SP;
        n.pub = list === pub;
      });
      // Exposed items in front (towards the viewer), private ones behind.
      place(pub, p.d / 2 - 50);
      place(priv, p.d / 2 - 50 - rowsPub * SP - (rowsPub ? 20 : 0));
    }
    // Place each depth row centered under its parents.
    const depths = Object.keys(byDepth).map(Number).sort((a, b) => a - b);
    for (const d of depths) {
      const row = byDepth[d].sort((a, b) => a.id.localeCompare(b.id));
      const total = row.reduce((s, p) => s + p.w, 0) + (row.length - 1) * 60;
      let x = -total / 2;
      for (const p of row) { p.x = x + p.w / 2; x += p.w + 60; p.y = -d * 320; }
    }
    for (const p of planes) for (const n of p.nodes) { n.x = p.x + n.lx; n.y = p.y; n.z = n.lz; n.plane = p; }
    const edges = [];
    for (const n of nodes.values()) {
      for (const c of n.it.calls) if (nodes.has(c.to)) edges.push({ a: n, b: nodes.get(c.to), kind: "call", inferred: c.inferred });
      if (showUses) for (const u of n.it.uses) if (nodes.has(u.to)) edges.push({ a: n, b: nodes.get(u.to), kind: "use" });
    }
    const ys = planes.map((p) => p.y);
    const cy = (Math.min(...ys) + Math.max(...ys)) / 2;
    for (const p of planes) p.y -= cy;
    for (const n of nodes.values()) n.y -= cy;
    model = { planes, nodes: [...nodes.values()], edges };
  }

  function project(x, y, z) {
    const cyw = Math.cos(cam.yaw), syw = Math.sin(cam.yaw);
    const cp = Math.cos(cam.pitch), sp = Math.sin(cam.pitch);
    let X = x * cyw - z * syw;
    let Z = x * syw + z * cyw;
    let Y = y * cp - Z * sp;
    Z = y * sp + Z * cp;
    const s = (cam.dist / (cam.dist - Z)) * cam.zoom;
    return { x: canvas.width / 2 + X * s, y: canvas.height / 2 - Y * s, s, z: Z };
  }

  function colors() {
    const cs = getComputedStyle(document.documentElement);
    const v = (n) => cs.getPropertyValue(n).trim();
    return { text: v("--text"), muted: v("--muted"), border: v("--border"), bg: v("--bg"), mod: v("--mod"), fn: v("--fn"), type: v("--type"), added: v("--added"), changed: v("--changed"), removed: v("--removed"), priv: v("--private"), hl: v("--highlight") };
  }

  function draw() {
    if (!model) return;
    const dpr = window.devicePixelRatio || 1;
    const r = canvas.getBoundingClientRect();
    if (canvas.width !== Math.round(r.width * dpr)) { canvas.width = Math.round(r.width * dpr); canvas.height = Math.round(r.height * dpr); }
    const C = colors();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.scale(dpr, dpr);
    const W = canvas.width / dpr, H = canvas.height / dpr;
    const P = (x, y, z) => { const p = project(x, y, z); return { x: (p.x - canvas.width / 2) / dpr + W / 2, y: (p.y - canvas.height / 2) / dpr + H / 2, s: p.s, z: p.z }; };
    const status = (it) => it.status === "added" ? C.added : it.status === "removed" ? C.removed : it.status === "changed" ? C.changed : C.muted;
    // Planes, far to near.
    const planes = model.planes.map((p) => {
      const corners = [[-p.w / 2, -p.d / 2], [p.w / 2, -p.d / 2], [p.w / 2, p.d / 2], [-p.w / 2, p.d / 2]].map(([dx, dz]) => P(p.x + dx, p.y, dz));
      return { p, corners, z: corners.reduce((s, c) => s + c.z, 0) / 4 };
    }).sort((a, b) => a.z - b.z);
    for (const { p, corners } of planes) {
      const dimmed = focus && focus !== p.id;
      ctx.globalAlpha = dimmed ? 0.25 : 1;
      ctx.beginPath();
      corners.forEach((c, i) => (i ? ctx.lineTo(c.x, c.y) : ctx.moveTo(c.x, c.y)));
      ctx.closePath();
      ctx.fillStyle = C.bg;
      ctx.globalAlpha = dimmed ? 0.12 : 0.72;
      ctx.fill();
      ctx.globalAlpha = dimmed ? 0.25 : 1;
      ctx.strokeStyle = C.mod;
      ctx.lineWidth = 1.4;
      if (items[p.id].vis_scope) ctx.setLineDash([6, 4]); else ctx.setLineDash([]);
      ctx.stroke();
      ctx.setLineDash([]);
      // Divider between the exposed and private halves.
      const priv = p.nodes.filter((n) => !n.pub);
      if (priv.length && p.nodes.some((n) => n.pub)) {
        const zDiv = Math.max(...priv.map((n) => n.lz)) + 35;
        const a = P(p.x - p.w / 2, p.y, zDiv), b = P(p.x + p.w / 2, p.y, zDiv);
        ctx.setLineDash([3, 4]);
        ctx.strokeStyle = C.priv;
        ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); ctx.stroke();
        ctx.setLineDash([]);
      }
      const lp = corners[0];
      ctx.fillStyle = C.mod;
      ctx.font = "600 13px " + getComputedStyle(document.body).getPropertyValue("--mono");
      ctx.fillText(`${modLabel(p.id)}  · depth ${p.depth}`, lp.x + 6, lp.y - 6);
    }
    ctx.globalAlpha = 1;
    // Edges.
    for (const e of model.edges) {
      const a = P(e.a.x, e.a.y, e.a.z), b = P(e.b.x, e.b.y, e.b.z);
      const cross = e.a.plane !== e.b.plane;
      const dimmed = focus && (e.a.plane.id !== focus && e.b.plane.id !== focus);
      const hot = hover && (hover === e.a || hover === e.b);
      ctx.globalAlpha = dimmed ? 0.08 : hot ? 1 : cross ? 0.85 : 0.6;
      // Crossing into a module and landing on a private item is only legal
      // towards ancestors (children see their parents' private items).
      const intoPrivate = cross && !e.b.pub;
      ctx.strokeStyle = intoPrivate ? C.priv : e.kind === "call" ? C.fn : C.type;
      ctx.lineWidth = hot ? 2.6 : cross ? 1.6 : 1.2;
      ctx.setLineDash(e.kind === "use" ? [2, 4] : e.inferred ? [6, 4] : cross ? [1, 3] : []);
      ctx.beginPath();
      if (cross) {
        // Bow vertical edges so they read as going between layers.
        const mx = (a.x + b.x) / 2 + (b.y - a.y) * 0.08, my = (a.y + b.y) / 2;
        ctx.moveTo(a.x, a.y); ctx.quadraticCurveTo(mx, my, b.x, b.y);
      } else { ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); }
      ctx.stroke();
      ctx.setLineDash([]);
      // Arrow head.
      const ang = Math.atan2(b.y - a.y, b.x - a.x);
      ctx.beginPath();
      ctx.moveTo(b.x - 9 * Math.cos(ang) + 4 * Math.sin(ang), b.y - 9 * Math.sin(ang) - 4 * Math.cos(ang));
      ctx.lineTo(b.x - 3 * Math.cos(ang), b.y - 3 * Math.sin(ang));
      ctx.lineTo(b.x - 9 * Math.cos(ang) - 4 * Math.sin(ang), b.y - 9 * Math.sin(ang) + 4 * Math.cos(ang));
      ctx.fillStyle = ctx.strokeStyle;
      ctx.fill();
    }
    ctx.globalAlpha = 1;
    // Nodes, far to near.
    projected = model.nodes.map((n) => ({ n, p: P(n.x, n.y, n.z) })).sort((a, b) => a.p.z - b.p.z);
    ctx.font = "12px " + getComputedStyle(document.body).getPropertyValue("--mono");
    for (const { n, p } of projected) {
      const dimmed = focus && n.plane.id !== focus;
      ctx.globalAlpha = dimmed ? 0.2 : n.context ? 0.55 : 1;
      const rad = Math.max(3, 6 * p.s);
      ctx.beginPath();
      ctx.arc(p.x, p.y, rad, 0, Math.PI * 2);
      if (n.pub) { ctx.fillStyle = status(n.it); ctx.fill(); }
      else { ctx.fillStyle = C.bg; ctx.fill(); ctx.strokeStyle = status(n.it); ctx.lineWidth = 2; ctx.stroke(); }
      if (hover === n) { ctx.strokeStyle = C.text; ctx.lineWidth = 2; ctx.beginPath(); ctx.arc(p.x, p.y, rad + 3, 0, Math.PI * 2); ctx.stroke(); }
      const crowded = model.nodes.length > 60;
      if (!crowded || hover === n || (focus && n.plane.id === focus) || (!n.context && n.plane.nodes.length <= 12)) {
        ctx.fillStyle = n.context ? C.muted : C.text;
        ctx.fillText(displayName(n.it), p.x + rad + 4, p.y + 4);
      }
      p.r = rad;
    }
    ctx.globalAlpha = 1;
  }

  function nodeAt(x, y) {
    for (let i = projected.length - 1; i >= 0; i--) {
      const { n, p } = projected[i];
      if ((p.x - x) ** 2 + (p.y - y) ** 2 < (p.r + 4) ** 2) return n;
    }
    return null;
  }

  function bar() {
    const b = $("#space-bar");
    const planes = model.planes.slice().sort((a, b) => a.depth - b.depth || a.id.localeCompare(b.id));
    b.innerHTML = `<b>Module planes</b>
      <button data-preset="side">Side view</button><button data-preset="tilt">Tilted</button><button data-preset="top">From above</button>
      <label>focus <select id="space-focus"><option value="">all planes</option>${planes.map((p) => `<option value="${esc(p.id)}" ${focus === p.id ? "selected" : ""}>${"· ".repeat(p.depth)}${esc(modLabel(p.id))}</option>`).join("")}</select></label>
      <label><input type="checkbox" id="space-uses" ${showUses ? "checked" : ""}> uses</label>
      <label><input type="checkbox" id="space-unchanged" ${showUnchanged ? "checked" : ""}> all items</label>
      <span style="flex:1"></span><span class="dim">drag to rotate · wheel to zoom · click a node to open it</span>`;
    for (const btn of b.querySelectorAll("[data-preset]")) btn.onclick = () => preset(btn.dataset.preset);
    $("#space-focus").onchange = (e) => { focus = e.target.value || null; if (focus) preset("top"); else draw(); };
    $("#space-unchanged").onchange = (e) => { showUnchanged = e.target.checked; build(); bar(); draw(); };
    $("#space-uses").onchange = (e) => { showUses = e.target.checked; build(); bar(); draw(); };
    $("#space-legend").innerHTML = `● exposed item &nbsp; ○ private item &nbsp; <span style="color:var(--fn)">━</span> call &nbsp; <span style="color:var(--type)">┅</span> use<br>Edges between planes cross a module boundary and land on exposed items;<br>grey ones reach a private item of an ancestor module.`;
  }

  function animateTo(target) {
    const start = { ...cam };
    const t0 = performance.now();
    const step = (t) => {
      const k = Math.min(1, (t - t0) / 450);
      const e = k * (2 - k);
      for (const key of Object.keys(target)) cam[key] = start[key] + (target[key] - start[key]) * e;
      draw();
      if (k < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }

  function preset(name) {
    if (name === "side") animateTo({ yaw: -0.25, pitch: 0.08 });
    if (name === "tilt") animateTo({ yaw: -0.6, pitch: 0.55 });
    if (name === "top") animateTo({ yaw: 0, pitch: 1.5 });
  }

  let dragging = null;
  canvas.addEventListener("pointerdown", (e) => { dragging = { x: e.clientX, y: e.clientY, yaw: cam.yaw, pitch: cam.pitch, moved: false }; canvas.setPointerCapture(e.pointerId); });
  canvas.addEventListener("pointermove", (e) => {
    const r = canvas.getBoundingClientRect();
    if (dragging) {
      const dx = e.clientX - dragging.x, dy = e.clientY - dragging.y;
      if (Math.abs(dx) + Math.abs(dy) > 3) dragging.moved = true;
      cam.yaw = dragging.yaw + dx * 0.006;
      cam.pitch = Math.max(-0.2, Math.min(1.55, dragging.pitch + dy * 0.005));
      draw();
      return;
    }
    const n = nodeAt(e.clientX - r.left, e.clientY - r.top);
    if (n !== hover) {
      hover = n;
      draw();
      const tip = $("#tooltip");
      if (n) {
        tip.innerHTML = `<div class="tp">${esc(n.it.id)}</div><div>${kindLabel(n.it.kind)} · ${statusHtml(n.it)} ${visHtml(n.it)}</div><pre>${esc(n.it.sig.split("\n").slice(0, 6).join("\n"))}</pre>`;
        tip.style.display = "block";
      } else tip.style.display = "none";
    }
    if (hover) { const tip = $("#tooltip"); tip.style.left = e.clientX + 14 + "px"; tip.style.top = e.clientY + 16 + "px"; }
  });
  canvas.addEventListener("pointerup", (e) => {
    const wasDrag = dragging?.moved;
    dragging = null;
    if (wasDrag) return;
    const r = canvas.getBoundingClientRect();
    const n = nodeAt(e.clientX - r.left, e.clientY - r.top);
    if (n) { $("#tooltip").style.display = "none"; openOnCanvas(n.it.id, { switchView: true }); }
  });
  canvas.addEventListener("wheel", (e) => { e.preventDefault(); cam.zoom = Math.max(0.3, Math.min(3, cam.zoom * Math.exp(-e.deltaY * 0.0012))); draw(); }, { passive: false });
  window.addEventListener("resize", () => S.view === "space" && draw());

  return {
    show() {
      build();
      bar();
      // Fit zoom to the model.
      const xs = model.planes.flatMap((p) => [p.x - p.w / 2, p.x + p.w / 2]);
      const span = Math.max(600, Math.max(...xs) - Math.min(...xs), model.planes.length * 120);
      cam.zoom = Math.min(1.4, (canvas.getBoundingClientRect().width * 0.85) / span);
      draw();
    },
  };
})();

// ---------------------------------------------------------------------------
// Boot

$("#pr-title").innerHTML = D.meta.title.includes(D.meta.base)
  ? `<b>${esc(D.meta.title)}</b>`
  : `<b>${esc(D.meta.title)}</b> &nbsp;${esc(D.meta.base)} → ${esc(D.meta.head)}`;
for (const b of document.querySelectorAll("#views button")) b.onclick = () => setView(b.dataset.view);
window.addEventListener("resize", () => S.view === "canvas" && drawEdges());
setView("overview");
