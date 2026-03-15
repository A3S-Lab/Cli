import { useEffect, useRef, useState } from 'react';
import { Agentation } from 'agentation';
import './index.css';

// ── Types ─────────────────────────────────────────────────────

type View = 'services' | 'box';

type SvcState =
  | 'running' | 'starting' | 'restarting'
  | 'stopped' | 'failed' | 'unhealthy' | 'pending';

interface StatusRow {
  name: string;
  state: SvcState;
  pid?: number;
  port: number;
  subdomain?: string;
  uptime_secs?: number;
  proxy_port: number;
}

interface LogEntry {
  id: number;
  service: string;
  line: string;
  ts: string;
}

// ── Utilities ─────────────────────────────────────────────────

const PALETTE = [
  '#22d3ee','#4ade80','#fbbf24','#f472b6',
  '#a78bfa','#60a5fa','#34d399','#fb923c',
];
const colorMap = new Map<string, string>();
let colorIdx = 0;

function getColor(name: string): string {
  if (!colorMap.has(name)) colorMap.set(name, PALETTE[colorIdx++ % PALETTE.length]);
  return colorMap.get(name)!;
}

function stripAnsi(s: string): string {
  return s.replace(/\x1b\[[0-9;]*[mGKHF]/g, '');
}

function highlight(text: string, query: string): React.ReactNode {
  if (!query) return text;
  const idx = text.toLowerCase().indexOf(query.toLowerCase());
  if (idx === -1) return text;
  return (
    <>
      {text.slice(0, idx)}
      <mark className="log-mark">{text.slice(idx, idx + query.length)}</mark>
      {text.slice(idx + query.length)}
    </>
  );
}

function fmtUptime(s: number): string {
  s = Math.floor(s);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m${s % 60}s`;
  return `${Math.floor(s / 3600)}h${Math.floor((s % 3600) / 60)}m`;
}

function nowTime(): string {
  return new Date().toLocaleTimeString('en', { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

// ── Hooks ─────────────────────────────────────────────────────

function useStatus(ms = 1500) {
  const [rows, setRows] = useState<StatusRow[]>([]);
  const [connected, setConnected] = useState(false);
  const [uptimeSecs, setUptimeSecs] = useState(0);
  const startRef = useRef(Date.now());

  useEffect(() => {
    let alive = true;
    async function poll() {
      try {
        const data: StatusRow[] = await fetch('/api/status').then(r => r.json());
        if (alive) { setRows(data); setConnected(true); setUptimeSecs(Math.floor((Date.now() - startRef.current) / 1000)); }
      } catch { if (alive) setConnected(false); }
    }
    poll();
    const id = setInterval(poll, ms);
    return () => { alive = false; clearInterval(id); };
  }, [ms]);

  return { rows, connected, uptimeSecs };
}

let logSeq = 0;

function useLogs(selected: string | null) {
  const [lines, setLines] = useState<LogEntry[]>([]);

  useEffect(() => {
    let alive = true;
    let es: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout>;
    setLines([]);

    function connect() {
      if (!alive) return;
      const url = '/api/logs' + (selected ? `?service=${encodeURIComponent(selected)}` : '');
      es = new EventSource(url);
      es.onmessage = e => {
        if (!alive) return;
        try {
          const m = JSON.parse(e.data) as { service: string; line: string };
          setLines(prev => {
            const next = [...prev, { ...m, id: logSeq++, ts: nowTime() }];
            return next.length > 2000 ? next.slice(-2000) : next;
          });
        } catch { /* ignore */ }
      };
      es.onerror = () => { es?.close(); if (alive) retry = setTimeout(connect, 2000); };
    }

    const histUrl = '/api/history' + (selected ? `?service=${encodeURIComponent(selected)}` : '');
    fetch(histUrl)
      .then(r => r.json())
      .then((data: { service: string; line: string }[]) => {
        if (!alive) return;
        setLines(data.map(d => ({ ...d, id: logSeq++, ts: nowTime() })));
        connect();
      })
      .catch(() => { if (alive) connect(); });

    return () => { alive = false; clearTimeout(retry); es?.close(); };
  }, [selected]);

  return lines;
}

// ── Components ────────────────────────────────────────────────

function SvcRow({
  row, active, onSelect, onRestart, onStop,
}: {
  row: StatusRow;
  active: boolean;
  onSelect: () => void;
  onRestart: () => void;
  onStop: () => void;
}) {
  const c = getColor(row.name);
  const url = row.subdomain
    ? `http://${row.subdomain}.localhost:${row.proxy_port}`
    : `http://localhost:${row.port}`;

  return (
    <div className={`svc-row${active ? ' active' : ''}`} onClick={onSelect}>
      <div className="svc-top">
        <div className={`svc-dot ${row.state}`} />
        <span className="svc-name" style={{ color: c }}>{row.name}</span>
        <span className={`svc-badge ${row.state}`}>{row.state}</span>
      </div>
      <div className="svc-bottom">
        <span className="svc-url">
          <a href={url} target="_blank" rel="noreferrer" onClick={e => e.stopPropagation()}>{url}</a>
        </span>
        <span className="svc-uptime">↑{row.uptime_secs != null ? fmtUptime(row.uptime_secs) : '—'}</span>
      </div>
      <div className="svc-actions">
        <button className="act-btn restart" onClick={e => { e.stopPropagation(); onRestart(); }}>restart</button>
        <button className="act-btn stop" onClick={e => { e.stopPropagation(); onStop(); }}>stop</button>
      </div>
    </div>
  );
}

function Sidebar({ rows, selected, onSelect, width, onWidthChange }: {
  rows: StatusRow[];
  selected: string | null;
  onSelect: (n: string) => void;
  width: number;
  onWidthChange: (w: number) => void;
}) {
  const dragging = useRef(false);
  const startX = useRef(0);
  const startW = useRef(0);

  function onMouseDown(e: React.MouseEvent) {
    dragging.current = true;
    startX.current = e.clientX;
    startW.current = width;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
  }

  useEffect(() => {
    function onMouseMove(e: MouseEvent) {
      if (!dragging.current) return;
      const next = Math.max(160, Math.min(480, startW.current + e.clientX - startX.current));
      onWidthChange(next);
    }
    function onMouseUp() {
      if (!dragging.current) return;
      dragging.current = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    }
    window.addEventListener('mousemove', onMouseMove);
    window.addEventListener('mouseup', onMouseUp);
    return () => { window.removeEventListener('mousemove', onMouseMove); window.removeEventListener('mouseup', onMouseUp); };
  }, [onWidthChange]);

  async function restart(name: string) { await fetch(`/api/restart/${encodeURIComponent(name)}`, { method: 'POST' }); }
  async function stop(name: string)    { await fetch(`/api/stop/${encodeURIComponent(name)}`, { method: 'POST' }); }

  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <span className="sidebar-label">Services</span>
        <span className="sidebar-count">{rows.length}</span>
      </div>
      <div className="svc-list">
        {rows.map(row => (
          <SvcRow
            key={row.name}
            row={row}
            active={selected === row.name}
            onSelect={() => onSelect(row.name)}
            onRestart={() => restart(row.name)}
            onStop={() => stop(row.name)}
          />
        ))}
      </div>
      <div className="sidebar-resize" onMouseDown={onMouseDown} />
    </aside>
  );
}

function LogPanel({ lines, selected, onAll }: { lines: LogEntry[]; selected: string | null; onAll: () => void }) {
  const bodyRef = useRef<HTMLDivElement>(null);
  const autoRef = useRef(true);
  const [autoScroll, setAutoScroll] = useState(true);
  const [filter, setFilter] = useState('');

  const filtered = filter
    ? lines.filter(e => stripAnsi(e.line).toLowerCase().includes(filter.toLowerCase()))
    : lines;

  useEffect(() => {
    if (autoRef.current && bodyRef.current) {
      bodyRef.current.scrollTop = bodyRef.current.scrollHeight;
    }
  }, [filtered]);

  function onScroll() {
    const el = bodyRef.current;
    if (!el) return;
    const near = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    autoRef.current = near;
    setAutoScroll(near);
  }

  return (
    <main className="log-panel">
      <div className="log-head">
        <span className="log-scope">
          logs / <span className="log-scope-name">{selected ?? 'all'}</span>
        </span>
        <div className="log-head-spacer" />
        <input
          className="log-filter"
          placeholder="filter…"
          value={filter}
          onChange={e => setFilter(e.target.value)}
          spellCheck={false}
        />
        <button className={`btn-all${!selected ? ' on' : ''}`} onClick={onAll}>all</button>
        <span className="scroll-hint" style={{ opacity: autoScroll ? 1 : 0.3 }}>↓ auto</span>
      </div>
      <div className="log-body" ref={bodyRef} onScroll={onScroll}>
        {filtered.length === 0 ? (
          <div className="log-empty">{lines.length === 0 ? 'waiting for output…' : 'no matches'}</div>
        ) : (
          filtered.map(entry => {
            const c = getColor(entry.service);
            const text = stripAnsi(entry.line);
            return (
              <div className="log-row" key={entry.id}>
                <span className="log-time">{entry.ts}</span>
                <span className="log-tag" style={{ color: c }}>[{entry.service}]</span>
                <span className="log-msg">{filter ? highlight(text, filter) : text}</span>
              </div>
            );
          })
        )}
      </div>
    </main>
  );
}

function Statusbar({ rows }: { rows: StatusRow[] }) {
  const run  = rows.filter(r => r.state === 'running').length;
  const stp  = rows.filter(r => r.state === 'stopped' || r.state === 'pending').length;
  const fail = rows.filter(r => r.state === 'failed' || r.state === 'unhealthy').length;

  return (
    <footer className="statusbar">
      <div className="stat-seg">
        <span className="stat-pip g" />
        <span className="stat-num g">{run}</span>
        <span>running</span>
      </div>
      <div className="stat-seg">
        <span className="stat-pip y" />
        <span className="stat-num y">{stp}</span>
        <span>stopped</span>
      </div>
      <div className="stat-seg">
        <span className="stat-pip r" />
        <span className="stat-num r">{fail}</span>
        <span>failed</span>
      </div>
      <div className="stat-spacer" />
      <span className="stat-host">{location.host}</span>
    </footer>
  );
}


// ── Box Panel ─────────────────────────────────────────────────

type BoxView = 'containers' | 'images' | 'networks' | 'volumes' | 'info';

interface BoxContainer { id: string; name: string; image: string; status: string; created: string; ports: string; command: string; }
interface BoxImage     { repository: string; tag: string; digest: string; size: string; pulled: string; reference: string; }
interface BoxNetwork   { name: string; driver: string; subnet: string; gateway: string; isolation: string; endpoints: string; }
interface BoxVolume    { driver: string; name: string; mount_point: string; in_use_by: string; }
interface BoxInfo      { version: string; virtualization: string; home: string; boxes_total: number; boxes_running: number; images_cached: string; }

function useBoxData<T>(url: string, ms = 4000) {
  const [data, setData] = useState<T | null>(null);
  useEffect(() => {
    let alive = true;
    async function poll() {
      try {
        const d: T = await fetch(url).then(r => r.json());
        if (alive) setData(d);
      } catch { /* ignore */ }
    }
    poll();
    const id = setInterval(poll, ms);
    return () => { alive = false; clearInterval(id); };
  }, [url, ms]);
  return data;
}

function BoxPanel() {
  const [view, setView] = useState<BoxView>('containers');
  const [showAll, setShowAll] = useState(false);
  const [selectedCtr, setSelectedCtr] = useState<string | null>(null);
  const [ctrLogs, setCtrLogs] = useState('');
  const logRef = useRef<HTMLDivElement>(null);

  const containers = useBoxData<BoxContainer[]>(`/api/box/containers?all=${showAll}`);
  const images     = useBoxData<BoxImage[]>('/api/box/images');
  const networks   = useBoxData<BoxNetwork[]>('/api/box/networks');
  const volumes    = useBoxData<BoxVolume[]>('/api/box/volumes');
  const info       = useBoxData<BoxInfo>('/api/box/info', 10000);

  useEffect(() => {
    if (!selectedCtr) { setCtrLogs(''); return; }
    let alive = true;
    async function load() {
      const text = await fetch(`/api/box/logs/${encodeURIComponent(selectedCtr)}?tail=300`).then(r => r.text()).catch(() => '');
      if (alive) setCtrLogs(text);
    }
    load();
    const id = setInterval(load, 4000);
    return () => { alive = false; clearInterval(id); };
  }, [selectedCtr]);

  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [ctrLogs]);

  const statusColor = (s: string) =>
    s.startsWith('Up') ? 'var(--green)' : s.startsWith('Exited') ? 'var(--red)' : 'var(--text3)';

  const navItems: { key: BoxView; icon: string; label: string; count?: number }[] = [
    { key: 'containers', icon: '▣', label: 'containers', count: containers?.length },
    { key: 'images',     icon: '◧', label: 'images',     count: images?.length },
    { key: 'networks',   icon: '⬡', label: 'networks',   count: networks?.length },
    { key: 'volumes',    icon: '◫', label: 'volumes',     count: volumes?.length },
    { key: 'info',       icon: '◎', label: 'system' },
  ];

  return (
    <main className="kube-panel">
      <div className="kube-head">
        <span className="log-scope">box / <span className="log-scope-name">a3s-box</span></span>
        <div className="log-head-spacer" />
        {view === 'containers' && (
          <label className="box-toggle">
            <input type="checkbox" checked={showAll} onChange={e => setShowAll(e.target.checked)} />
            show all
          </label>
        )}
        {info && (
          <span style={{ fontFamily: 'var(--mono)', fontSize: 10, color: 'var(--text3)' }}>
            v{info.version} · {info.boxes_running}/{info.boxes_total} running
          </span>
        )}
      </div>

      <div className="kube-layout">
        {/* Nav */}
        <nav className="kube-nav">
          {navItems.map(item => (
            <button key={item.key} className={`kube-nav-item${view === item.key ? ' active' : ''}`} onClick={() => setView(item.key)}>
              <span className="kube-nav-icon">{item.icon}</span>
              {item.label}
              {item.count !== undefined && <span className="kube-nav-badge">{item.count ?? '…'}</span>}
            </button>
          ))}
          {info && (
            <div className="kube-nav-stats">
              <div className="kube-stat">
                <span className="kube-stat-dot" style={{ background: 'var(--green)' }} />
                <span className="kube-stat-val" style={{ color: 'var(--green)' }}>{info.boxes_running}</span>
                <span className="kube-stat-label">running</span>
              </div>
              <div className="kube-stat">
                <span className="kube-stat-dot" style={{ background: 'var(--cyan)' }} />
                <span className="kube-stat-val" style={{ color: 'var(--cyan)' }}>{info.boxes_total}</span>
                <span className="kube-stat-label">total</span>
              </div>
            </div>
          )}
        </nav>

        {/* Content */}
        <div className="kube-content">
          {view === 'containers' && (
            <div className="kube-section kube-pods-section">
              <div className="kube-section-head">
                <span className="kube-section-title">containers</span>
                <span className="kube-section-count">{containers?.length ?? '…'}</span>
                {selectedCtr && <button className="kube-close-logs" onClick={() => setSelectedCtr(null)}>✕ close logs</button>}
              </div>
              <div className="kube-pods-layout">
                <table className="kube-table">
                  <thead><tr><th>name</th><th>image</th><th>status</th><th>ports</th><th></th></tr></thead>
                  <tbody>
                    {!containers ? <tr><td colSpan={5} className="kube-empty-row">loading…</td></tr>
                    : containers.length === 0 ? <tr><td colSpan={5} className="kube-empty-row">no containers</td></tr>
                    : containers.map(c => {
                      const isSel = selectedCtr === c.id;
                      return (
                        <tr key={c.id} className={isSel ? 'kube-row-selected' : ''} style={{ cursor: 'pointer' }}
                          onClick={() => setSelectedCtr(isSel ? null : c.id)}>
                          <td className="kube-cell-name">{c.name}</td>
                          <td className="kube-cell-dim" style={{ maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{c.image}</td>
                          <td><span className="kube-pill" style={{ color: statusColor(c.status), background: c.status.startsWith('Up') ? 'rgba(74,222,128,0.1)' : 'rgba(248,113,113,0.1)' }}>{c.status}</span></td>
                          <td className="kube-cell-dim">{c.ports || '—'}</td>
                          <td style={{ display: 'flex', gap: 4 }}>
                            <button className="kube-del-btn" style={{ opacity: 1, color: 'var(--yellow)' }}
                              onClick={e => { e.stopPropagation(); fetch(`/api/box/stop/${encodeURIComponent(c.id)}`, { method: 'POST' }); }}
                              title="stop">■</button>
                            <button className="kube-del-btn"
                              onClick={e => { e.stopPropagation(); fetch(`/api/box/container/${encodeURIComponent(c.id)}`, { method: 'DELETE' }); }}
                              title="remove">✕</button>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
                {selectedCtr && (
                  <div className="kube-log-drawer">
                    <div className="kube-log-drawer-head">
                      <span className="kube-log-drawer-title">{selectedCtr}</span>
                    </div>
                    <div className="kube-log-body" ref={logRef}>
                      {ctrLogs
                        ? ctrLogs.split('\n').map((l, i) => <div key={i} className="kube-log-line">{l}</div>)
                        : <div className="kube-log-empty">no logs</div>}
                    </div>
                  </div>
                )}
              </div>
            </div>
          )}

          {view === 'images' && (
            <div className="kube-section">
              <div className="kube-section-head">
                <span className="kube-section-title">images</span>
                <span className="kube-section-count">{images?.length ?? '…'}</span>
              </div>
              <table className="kube-table">
                <thead><tr><th>repository</th><th>tag</th><th>size</th><th>pulled</th><th></th></tr></thead>
                <tbody>
                  {!images ? <tr><td colSpan={5} className="kube-empty-row">loading…</td></tr>
                  : images.length === 0 ? <tr><td colSpan={5} className="kube-empty-row">no images cached</td></tr>
                  : images.map(img => (
                    <tr key={img.reference}>
                      <td className="kube-cell-name">{img.repository}</td>
                      <td><span className="kube-ns-tag">{img.tag || 'latest'}</span></td>
                      <td className="kube-cell-dim">{img.size}</td>
                      <td className="kube-cell-dim">{img.pulled}</td>
                      <td>
                        <button className="kube-del-btn"
                          onClick={() => fetch(`/api/box/image/${encodeURIComponent(img.reference)}`, { method: 'DELETE' })}
                          title="remove">✕</button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {view === 'networks' && (
            <div className="kube-section">
              <div className="kube-section-head">
                <span className="kube-section-title">networks</span>
                <span className="kube-section-count">{networks?.length ?? '…'}</span>
              </div>
              <table className="kube-table">
                <thead><tr><th>name</th><th>driver</th><th>subnet</th><th>gateway</th><th>isolation</th><th>endpoints</th><th></th></tr></thead>
                <tbody>
                  {!networks ? <tr><td colSpan={7} className="kube-empty-row">loading…</td></tr>
                  : networks.length === 0 ? <tr><td colSpan={7} className="kube-empty-row">no networks</td></tr>
                  : networks.map(n => (
                    <tr key={n.name}>
                      <td className="kube-cell-name">{n.name}</td>
                      <td className="kube-cell-dim">{n.driver}</td>
                      <td className="kube-cell-dim">{n.subnet}</td>
                      <td className="kube-cell-dim">{n.gateway}</td>
                      <td className="kube-cell-dim">{n.isolation}</td>
                      <td className="kube-cell-dim">{n.endpoints}</td>
                      <td>
                        <button className="kube-del-btn"
                          onClick={() => fetch(`/api/box/network/${encodeURIComponent(n.name)}`, { method: 'DELETE' })}
                          title="remove">✕</button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {view === 'volumes' && (
            <div className="kube-section">
              <div className="kube-section-head">
                <span className="kube-section-title">volumes</span>
                <span className="kube-section-count">{volumes?.length ?? '…'}</span>
              </div>
              <table className="kube-table">
                <thead><tr><th>name</th><th>driver</th><th>mount point</th><th>in use by</th><th></th></tr></thead>
                <tbody>
                  {!volumes ? <tr><td colSpan={5} className="kube-empty-row">loading…</td></tr>
                  : volumes.length === 0 ? <tr><td colSpan={5} className="kube-empty-row">no volumes</td></tr>
                  : volumes.map(v => (
                    <tr key={v.name}>
                      <td className="kube-cell-name">{v.name}</td>
                      <td className="kube-cell-dim">{v.driver}</td>
                      <td className="kube-cell-dim">{v.mount_point}</td>
                      <td className="kube-cell-dim">{v.in_use_by || '—'}</td>
                      <td>
                        <button className="kube-del-btn"
                          onClick={() => fetch(`/api/box/volume/${encodeURIComponent(v.name)}`, { method: 'DELETE' })}
                          title="remove">✕</button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {view === 'info' && (
            <div className="kube-section">
              <div className="kube-section-head">
                <span className="kube-section-title">system info</span>
              </div>
              {!info ? <div className="log-empty">loading…</div> : (
                <div className="box-info-grid">
                  {[
                    ['version',        info.version],
                    ['virtualization', info.virtualization],
                    ['home',           info.home],
                    ['containers',     `${info.boxes_running} running / ${info.boxes_total} total`],
                    ['images',         info.images_cached],
                  ].map(([k, v]) => (
                    <div key={k} className="box-info-row">
                      <span className="box-info-key">{k}</span>
                      <span className="box-info-val">{v}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </main>
  );
}

// ── Topbar with nav ───────────────────────────────────────────

function Topbar({ rows, connected, uptimeSecs, view, onView }: {
  rows: StatusRow[];
  connected: boolean;
  uptimeSecs: number;
  view: View;
  onView: (v: View) => void;
}) {
  return (
    <header className="topbar">
      <span className="wordmark">
        <span className="wordmark-accent">a3s</span>
      </span>
      <nav className="topbar-nav">
        <button className={`nav-tab${view === 'services' ? ' active' : ''}`} onClick={() => onView('services')}>
          services <span className="nav-count">{rows.length}</span>
        </button>
        <button className={`nav-tab${view === 'box' ? ' active' : ''}`} onClick={() => onView('box')}>
          box
        </button>
      </nav>
      <div className="topbar-spacer" />
      <div className="conn-status">
        <div className={`conn-dot ${connected ? 'live' : 'dead'}`} />
        <span>{connected ? fmtUptime(uptimeSecs) : 'offline'}</span>
      </div>
    </header>
  );
}

// ── App ───────────────────────────────────────────────────────

export default function App() {
  const { rows, connected, uptimeSecs } = useStatus();
  const [selected, setSelected] = useState<string | null>(null);
  const [sidebarWidth, setSidebarWidth] = useState(256);
  const [view, setView] = useState<View>('services');
  const lines = useLogs(view === 'services' ? selected : null);

  return (
    <div className="shell" style={{ gridTemplateColumns: `${sidebarWidth}px 1fr` }}>
      <Topbar rows={rows} connected={connected} uptimeSecs={uptimeSecs} view={view} onView={setView} />
      {view === 'services' ? (
        <>
          <Sidebar rows={rows} selected={selected} onSelect={setSelected} width={sidebarWidth} onWidthChange={setSidebarWidth} />
          <LogPanel lines={lines} selected={selected} onAll={() => setSelected(null)} />
        </>
      ) : (
        <BoxPanel />
      )}
      <Statusbar rows={rows} />
      {import.meta.env.DEV && <Agentation />}
    </div>
  );
}
