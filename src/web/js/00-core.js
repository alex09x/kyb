    // Global Application State
    const App = {
      entries: [],
      incidents: [],
      tasks: [],
      tags: [],
      activity: [],
      audit: [],
      health: {},
      selectedKey: null,
      currentFilter: 'all',
      searchQuery: '',
      activeTab: 'search',
      incidentStatusFilter: 'all',
      incidentSeverityFilter: 'all',
      incidentQueue: null,
      taskQueue: null,
      taskStatusFilter: 'live',
      taskSort: 'newest',   // 'newest' | 'due' (the by-date view)
      deadlines: new Map(), // task key -> its deadline as GET /deadlines answered
      deadlineBlock: null,  // the deadlines block of the last read answer (the strip)
      taskCursor: 0,
      taskKeyNav: false,
      incidentCursor: 0,
      incidentKeyNav: false,
      activityFilter: 'all',
      feedPaused: false,
      places: { services: [], hosts: [] },
      place: null,          // {kind: 'service'|'host', name}
      impact: null,
      related: new Map(),   // key -> {outgoing, incoming}
      lastSyncTime: Date.now(),
      newModalKind: 'knowledge',
      activeResolveKey: null,
      pendingHighlight: null,
      isBackendSearchResults: false,
      currentHistoryList: []
    };

    // Toast Notification System
    function showToast(message, type = 'info') {
      const container = document.getElementById('toastContainer');
      const toast = document.createElement('div');
      toast.className = `toast ${type}`;
      const icon = type === 'success' ? '✔' : type === 'error' ? '✖' : 'ℹ';
      toast.innerHTML = `<span style="font-weight:700;">${icon}</span><span>${escapeHtml(message)}</span>`;
      container.appendChild(toast);
      setTimeout(() => {
        toast.style.transition = 'opacity 0.2s ease, transform 0.2s ease';
        toast.style.opacity = '0';
        toast.style.transform = 'translateY(10px)';
        setTimeout(() => toast.remove(), 200);
      }, 3000);
    }

    // Markdown Parser
    function renderMarkdown(md) {
      if (!md) return '';
      let html = md
        .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
        .replace(/```([a-z0-9_-]*)\n([\s\S]*?)```/g, (_, lang, code) => `<pre><code class="lang-${lang}">${code.trim()}</code></pre>`)
        .replace(/`([^`]+)`/g, '<code>$1</code>')
        .replace(/^### (.*$)/gim, '<h3>$1</h3>')
        .replace(/^## (.*$)/gim, '<h2>$1</h2>')
        .replace(/^# (.*$)/gim, '<h1>$1</h1>')
        .replace(/^&gt; (.*$)/gim, '<blockquote>$1</blockquote>')  // '>' was escaped two lines up
        .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
        .replace(/\*([^*]+)\*/g, '<em>$1</em>')
        .replace(/- \[x\] (.*$)/gim, '<div style="display:flex;align-items:center;gap:6px;margin:3px 0;"><input type="checkbox" checked disabled> <span>$1</span></div>')
        .replace(/- \[ \] (.*$)/gim, '<div style="display:flex;align-items:center;gap:6px;margin:3px 0;"><input type="checkbox" disabled> <span>$1</span></div>')
        .replace(/^\- (.*$)/gim, '<li>$1</li>')
        .replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_, label, url) => {
          let href = url.trim();
          let target = ' target="_blank" rel="noopener noreferrer"';
          if (href.startsWith('#/')) {
            target = '';
          } else if (!href.startsWith('http://') && !href.startsWith('https://') && !href.startsWith('mailto:') && !href.startsWith('/')) {
            href = '#/knowledge/' + encodeURIComponent(href);
            target = '';
          }
          return `<a href="${href}"${target} style="color:var(--accent); text-decoration:underline;">${label}</a>`;
        })
        .replace(/\n\n+/g, '</p><p>');

      return '<p>' + html + '</p>';
    }

    // Relative Time Formatter
    function timeAgo(dateString) {
      if (!dateString) return 'recently';
      const date = new Date(dateString);
      if (isNaN(date)) return dateString;
      const seconds = Math.floor((new Date() - date) / 1000);
      if (seconds < 60) return `${Math.max(1, seconds)}s ago`;
      const minutes = Math.floor(seconds / 60);
      if (minutes < 60) return `${minutes}m ago`;
      const hours = Math.floor(minutes / 60);
      if (hours < 24) return `${hours}h ago`;
      const days = Math.floor(hours / 24);
      return `${days}d ago`;
    }

    // API Helpers
    async function apiFetch(endpoint) {
      try {
        const res = await fetch(endpoint, { headers: { ...authHeaders(), 'x-kyb-today': localToday() } });
        if (res.status === 401) askForToken();
        if (!res.ok) return null;
        return await res.json();
      } catch (err) {
        console.error('API GET Error', endpoint, err);
        return null;
      }
    }

    async function apiPost(endpoint, body) {
      try {
        const res = await fetch(endpoint, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json', ...authHeaders() },
          body: JSON.stringify(body)
        });
        const data = await res.json().catch(() => ({}));
        if (res.status === 401) askForToken();
        // A write changes the record and its history, and possibly the entries
        // linked to it; whatever the detail pane cached is now a stale version.
        if (res.ok) { App.cacheGen++; App.detailCache.clear(); if (App.entryPrefetch) App.entryPrefetch.clear(); }
        return { ok: res.ok, status: res.status, data };
      } catch (err) {
        console.error('API POST Error', endpoint, err);
        return { ok: false, error: err.message };
      }
    }

    // ---- Who is at the console. The token lives in this browser only and goes out as a
    // bearer header; the server decides what it may do. The role only hides controls that
    // would be refused anyway.
    function storedToken() { try { return localStorage.getItem('kyb.token') || ''; } catch (_) { return ''; } }
    function authHeaders() { const t = storedToken(); return t ? { Authorization: 'Bearer ' + t } : {}; }
    function readOnly() { return !!App.me && App.me.role === 'read'; }
    function meName() { return (App.me && App.me.actor && App.me.actor.name) || ''; }
    // Entry points of every write call this first: the keyboard reaches them too.
    function refuseReadOnly() {
      if (!readOnly()) return false;
      showToast('Read-only: this token has the read role', 'error');
      return true;
    }

    async function loadWhoami() {
      const me = await apiFetch('/whoami');
      App.me = me || App.me || null;
      document.body.dataset.role = App.me ? App.me.role : '';
      const who = document.getElementById('whoLabel');
      if (who) who.textContent = !App.me || App.me.auth === 'off' ? '' : meName() ? `· ${meName()}${readOnly() ? ' (read-only)' : ''}` : readOnly() ? '· read-only' : '· no token';
      return App.me;
    }

    function askForToken() {
      if (App.askedForToken) return;      // once per failure, not once per request of a refresh
      App.askedForToken = true;
      openTokenModal(storedToken() ? 'This token is not known to the server, or was revoked.' : 'This server asks for a token.');
    }

    function openTokenModal(why) {
      document.getElementById('tokenInput').value = '';
      const mode = App.me ? App.me.auth : '';
      document.getElementById('tokenWhy').textContent = why || (mode === 'off' ? 'This server does not ask for tokens; one is not needed here.' : meName() ? `Signed in as ${meName()}, role ${App.me.role}.` : 'Paste the token you were issued. It stays in this browser.');
      document.getElementById('tokenErr').textContent = '';
      openModal('tokenModal');
    }

    async function submitToken() {
      const t = document.getElementById('tokenInput').value.trim();
      if (!markRequired(['tokenInput'])) return;
      // tried before it is kept: a token the server does not know is not stored
      let res = null;
      try { res = await fetch('/whoami', { headers: { Authorization: 'Bearer ' + t } }); } catch (_) { res = null; }
      if (!res || !res.ok) {
        document.getElementById('tokenErr').textContent = res && res.status === 401 ? 'The server does not know this token.' : 'Could not reach the server.';
        return;
      }
      try { localStorage.setItem('kyb.token', t); } catch (_) {}
      App.askedForToken = false;
      closeModal('tokenModal');
      await loadWhoami();
      showToast(meName() ? `Signed in as ${meName()}` : 'Token saved', 'success');
      await refreshAll();
    }

    async function forgetToken() {
      try { localStorage.removeItem('kyb.token'); } catch (_) {}
      App.askedForToken = false;
      closeModal('tokenModal');
      await loadWhoami();
      await refreshAll();
    }

    // Full Refresh Loop
    async function refreshAll() {
      const [searchData, incData, taskData, tagsData, healthData, actData, auditData, placesData, dlData] = await Promise.all([
        apiFetch('/search?limit=100'),
        apiFetch('/incidents?all=1&limit=500'),
        apiFetch('/tasks?all=1&limit=500'),
        apiFetch('/tags'),
        apiFetch('/healthz'),
        apiFetch('/api/activity'),
        // the request log (who searched for what, from where) is the admin's; others are not shown it and do not ask
        App.feedPaused || (App.me && App.me.role !== 'admin') ? Promise.resolve(null) : apiFetch('/api/audit?limit=400'),
        apiFetch('/places'),
        apiFetch('/deadlines')
      ]);
      if (placesData && placesData.services) App.places = placesData;
      // a server without deadlines answers /deadlines with 404: no chips, no strip
      App.deadlines = new Map(((dlData && dlData.deadlines) || []).map(d => [d.key, d]));
      if (searchData) App.deadlineBlock = searchData.deadlines || null;

      if (!App.isBackendSearchResults && searchData && searchData.hits) App.entries = searchData.hits;
      if (incData && incData.incidents) App.incidents = incData.incidents;
      if (taskData && taskData.tasks) App.tasks = taskData.tasks;
      if (tagsData && tagsData.tags) App.tags = tagsData.tags;
      if (healthData) App.health = healthData;
      if (actData && actData.activity) App.activity = actData.activity;
      if (auditData && auditData.entries) App.audit = auditData.entries;

      App.lastSyncTime = Date.now();
      updateLastSyncTicker();

      renderHeaderStats();
      renderDeadlineStrip();
      renderKnowledgeList();
      renderIncidents(true);
      renderTasks(true);
      if (!App.feedPaused) renderFeed();
      renderPlaces();
    }

    function manualRefresh() {
      const btn = document.getElementById('refreshBtn');
      btn.style.transition = 'transform 0.5s ease';
      btn.style.transform = 'rotate(360deg)';
      setTimeout(() => { btn.style.transform = 'rotate(0deg)'; }, 500);
      refreshAll().then(() => showToast('Synced fleet memory and audit trail', 'success'));
    }

    function updateLastSyncTicker() {
      const el = document.getElementById('statLastSync');
      if (el) el.textContent = timeAgo(new Date(App.lastSyncTime).toISOString());
    }
    setInterval(updateLastSyncTicker, 2000);

    function renderHeaderStats() {
      const openInc = App.incidents.filter(i => i.status !== 'resolved').length;
      const activeTasks = App.tasks.filter(t => t.status !== 'done').length;

      document.getElementById('badgeKnowledgeCount').textContent = App.entries.length;
      const incBadge = document.getElementById('badgeIncidentsCount');
      incBadge.textContent = openInc;
      incBadge.className = openInc > 0 ? 'tab-badge red' : 'tab-badge';

      document.getElementById('badgeTasksCount').textContent = activeTasks;
      const totalOps = (App.activity && App.activity.length) || (App.audit && App.audit.length) || 0;
      document.getElementById('badgeFeedCount').textContent = totalOps;

      // Tag Pills in Explorer (Top 8 to keep view calm)
      const tagContainer = document.getElementById('tagPills');
      tagContainer.innerHTML = App.tags.slice(0, 8).map(t =>
        `<a class="badge-tag" onclick="filterByTag(${jsArg(t.tag)})">#${escapeHtml(t.tag)} <span style="opacity:0.6">${t.count}</span></a>`
      ).join('');
    }

