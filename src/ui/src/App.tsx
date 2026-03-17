import { useEffect, useRef, useState } from 'react';
import { Agentation } from 'agentation';
import './index.css';

// ── Types ─────────────────────────────────────────────────────

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

interface BoxContainer {
  id: string;
  name: string;
  image: string;
  status: string;
  created: string;
  ports: string;
  command: string;
}

interface BoxImage {
  repository: string;
  tag: string;
  digest: string;
  size: string;
  pulled: string;
  reference: string;
}

interface BoxNetwork {
  name: string;
  driver: string;
  subnet: string;
  gateway: string;
  isolation: string;
  endpoints: string;
}

interface BoxVolume {
  driver: string;
  name: string;
  mount_point: string;
  in_use_by: string;
}

interface BoxInfo {
  version: string;
  virtualization: string;
  home: string;
  boxes_total: number;
  boxes_running: number;
  images_cached: string;
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

// ── Box panel ─────────────────────────────────────────────────

type BoxSubTab = 'containers' | 'images' | 'networks' | 'volumes' | 'info';

function BoxPanel() {
  const [subTab, setSubTab] = useState<BoxSubTab>('containers');
  const [showAll, setShowAll] = useState(false);
  const [containers, setContainers] = useState<BoxContainer[]>([]);
  const [images, setImages]         = useState<BoxImage[]>([]);
  const [networks, setNetworks]     = useState<BoxNetwork[]>([]);
  const [volumes, setVolumes]       = useState<BoxVolume[]>([]);
  const [info, setInfo]             = useState<BoxInfo | null>(null);
  const [err, setErr]               = useState<string | null>(null);

  async function load() {
    setErr(null);
    try {
      if (subTab === 'containers') {
        const data = await fetch(`/api/box/containers${showAll ? '?all=true' : ''}`).then(r => r.json());
        setContainers(Array.isArray(data) ? data : []);
      } else if (subTab === 'images') {
        const data = await fetch('/api/box/images').then(r => r.json());
        setImages(Array.isArray(data) ? data : []);
      } else if (subTab === 'networks') {
        const data = await fetch('/api/box/networks').then(r => r.json());
        setNetworks(Array.isArray(data) ? data : []);
      } else if (subTab === 'volumes') {
        const data = await fetch('/api/box/volumes').then(r => r.json());
        setVolumes(Array.isArray(data) ? data : []);
      } else if (subTab === 'info') {
        const data = await fetch('/api/box/info').then(r => r.json());
        setInfo(data);
      }
    } catch (e) {
      setErr(String(e));
    }
  }

  useEffect(() => { load(); }, [subTab, showAll]);

  async function stopContainer(id: string) {
    await fetch(`/api/box/stop/${encodeURIComponent(id)}`, { method: 'POST' });
    load();
  }

  async function removeContainer(id: string) {
    await fetch(`/api/box/container/${encodeURIComponent(id)}`, { method: 'DELETE' });
    load();
  }

  async function removeImage(reference: string) {
    await fetch(`/api/box/image/${encodeURIComponent(reference)}`, { method: 'DELETE' });
    load();
  }

  async function removeNetwork(name: string) {
    await fetch(`/api/box/network/${encodeURIComponent(name)}`, { method: 'DELETE' });
    load();
  }

  async function removeVolume(name: string) {
    await fetch(`/api/box/volume/${encodeURIComponent(name)}`, { method: 'DELETE' });
    load();
  }

  const SUB_TABS: BoxSubTab[] = ['containers', 'images', 'networks', 'volumes', 'info'];

  return (
    <div className="box-panel">
      <div className="box-head">
        <div className="box-sub-tabs">
          {SUB_TABS.map(t => (
            <button
              key={t}
              className={`box-sub-tab${subTab === t ? ' active' : ''}`}
              onClick={() => setSubTab(t)}
            >{t}</button>
          ))}
        </div>
        <div className="box-head-spacer" />
        {subTab === 'containers' && (
          <label className="box-toggle">
            <input type="checkbox" checked={showAll} onChange={e => setShowAll(e.target.checked)} />
            show all
          </label>
        )}
        <button className="box-sub-tab" style={{ marginLeft: 8 }} onClick={load}>↻ refresh</button>
      </div>

      {err && <div className="box-err">Error: {err}</div>}

      {subTab === 'containers' && !err && (
        <div className="box-table-wrap">
          {containers.length === 0 ? (
            <div className="box-empty">no containers{showAll ? '' : ' — try "show all"'}</div>
          ) : (
            <table className="box-table">
              <thead>
                <tr>
                  <th>ID</th>
                  <th>Name</th>
                  <th>Image</th>
                  <th>Status</th>
                  <th>Ports</th>
                  <th>Created</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {containers.map(c => {
                  const running = c.status.toLowerCase().startsWith('up') || c.status.toLowerCase() === 'running';
                  return (
                    <tr key={c.id}>
                      <td style={{ color: 'var(--text3)', fontSize: 10 }}>{c.id.slice(0, 12)}</td>
                      <td style={{ color: 'var(--text)' }}>{c.name}</td>
                      <td>{c.image}</td>
                      <td className={running ? 'box-status-run' : 'box-status-stop'}>{c.status}</td>
                      <td>{c.ports || '—'}</td>
                      <td style={{ color: 'var(--text3)' }}>{c.created}</td>
                      <td style={{ whiteSpace: 'nowrap' }}>
                        {running && (
                          <button className="box-act-btn stop-btn" onClick={() => stopContainer(c.id)}>stop</button>
                        )}
                        <button className="box-act-btn danger" onClick={() => removeContainer(c.id)}>rm</button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
      )}

      {subTab === 'images' && !err && (
        <div className="box-table-wrap">
          {images.length === 0 ? (
            <div className="box-empty">no images</div>
          ) : (
            <table className="box-table">
              <thead>
                <tr>
                  <th>Repository</th>
                  <th>Tag</th>
                  <th>Size</th>
                  <th>Pulled</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {images.map((img, i) => (
                  <tr key={i}>
                    <td style={{ color: 'var(--text)' }}>{img.repository}</td>
                    <td>{img.tag || 'latest'}</td>
                    <td style={{ color: 'var(--text3)' }}>{img.size}</td>
                    <td style={{ color: 'var(--text3)' }}>{img.pulled}</td>
                    <td>
                      <button
                        className="box-act-btn danger"
                        onClick={() => removeImage(img.reference || `${img.repository}:${img.tag}`)}
                      >rmi</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}

      {subTab === 'networks' && !err && (
        <div className="box-table-wrap">
          {networks.length === 0 ? (
            <div className="box-empty">no networks</div>
          ) : (
            <table className="box-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Driver</th>
                  <th>Subnet</th>
                  <th>Gateway</th>
                  <th>Isolation</th>
                  <th>Endpoints</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {networks.map((n, i) => (
                  <tr key={i}>
                    <td style={{ color: 'var(--text)' }}>{n.name}</td>
                    <td>{n.driver}</td>
                    <td style={{ color: 'var(--text3)' }}>{n.subnet || '—'}</td>
                    <td style={{ color: 'var(--text3)' }}>{n.gateway || '—'}</td>
                    <td style={{ color: 'var(--text3)' }}>{n.isolation || '—'}</td>
                    <td style={{ color: 'var(--text3)' }}>{n.endpoints || '—'}</td>
                    <td>
                      <button className="box-act-btn danger" onClick={() => removeNetwork(n.name)}>rm</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}

      {subTab === 'volumes' && !err && (
        <div className="box-table-wrap">
          {volumes.length === 0 ? (
            <div className="box-empty">no volumes</div>
          ) : (
            <table className="box-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Driver</th>
                  <th>Mount Point</th>
                  <th>In Use By</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {volumes.map((v, i) => (
                  <tr key={i}>
                    <td style={{ color: 'var(--text)' }}>{v.name}</td>
                    <td>{v.driver}</td>
                    <td style={{ color: 'var(--text3)', fontSize: 10 }}>{v.mount_point || '—'}</td>
                    <td style={{ color: 'var(--text3)' }}>{v.in_use_by || '—'}</td>
                    <td>
                      <button className="box-act-btn danger" onClick={() => removeVolume(v.name)}>rm</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}

      {subTab === 'info' && !err && (
        <div className="box-info-wrap">
          {!info ? (
            <div className="box-empty">loading…</div>
          ) : (
            <div className="box-info-grid">
              <div className="box-info-row">
                <span className="box-info-key">Version</span>
                <span className="box-info-val">{info.version || '—'}</span>
              </div>
              <div className="box-info-row">
                <span className="box-info-key">Virtualization</span>
                <span className="box-info-val">{info.virtualization || '—'}</span>
              </div>
              <div className="box-info-row">
                <span className="box-info-key">Home</span>
                <span className="box-info-val">{info.home || '—'}</span>
              </div>
              <div className="box-info-row">
                <span className="box-info-key">Boxes</span>
                <span className="box-info-val">{info.boxes_total} total, {info.boxes_running} running</span>
              </div>
              <div className="box-info-row">
                <span className="box-info-key">Images Cached</span>
                <span className="box-info-val">{info.images_cached || '—'}</span>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
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

// ── Topbar ────────────────────────────────────────────────────

type AppTab = 'services' | 'box';

function Topbar({ rows, connected, uptimeSecs, tab, onTabChange }: {
  rows: StatusRow[];
  connected: boolean;
  uptimeSecs: number;
  tab: AppTab;
  onTabChange: (t: AppTab) => void;
}) {
  return (
    <header className="topbar">
      <span className="wordmark">
        <span className="wordmark-accent">a3s</span>
      </span>
      <nav className="topbar-nav">
        <button className={`nav-tab${tab === 'services' ? ' active' : ''}`} onClick={() => onTabChange('services')}>
          services <span className="nav-count">{rows.length}</span>
        </button>
        <button className={`nav-tab${tab === 'box' ? ' active' : ''}`} onClick={() => onTabChange('box')}>
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
  const [tab, setTab] = useState<AppTab>('services');
  const lines = useLogs(tab === 'services' ? selected : null);

  return (
    <div className="shell" style={{ gridTemplateColumns: `${sidebarWidth}px 1fr` }}>
      <Topbar rows={rows} connected={connected} uptimeSecs={uptimeSecs} tab={tab} onTabChange={setTab} />
      {tab === 'services' ? (
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
