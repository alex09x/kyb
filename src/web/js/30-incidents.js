    // Incidents Rendering & Actions
    // Incidents: a heat grid (service x age) over a command surface. The grid is
    // the summary, the command line is the filter made literal, the rows are the
    // answer. Everything below the grid is keyboard-driven (j k Enter r d h Esc).
    const INC_BUCKETS = [
      { id: 'h1',   label: '< 1h',      max: 3600e3 },
      { id: 'today',label: 'today',     max: 86400e3 },
      { id: 'd3',   label: '2-3d',      max: 3 * 86400e3 },
      { id: 'week', label: 'this week', max: 7 * 86400e3 },
      { id: 'older',label: 'older',     max: Infinity },
    ];
    function incWhen(i) { return new Date(i.detected_at || i.started_at || i.updated_at || 0).getTime(); }
    function incAgeLabel(i) {
      const ms = Date.now() - incWhen(i);
      if (!isFinite(ms) || ms < 0) return '';
      const m = Math.floor(ms / 60e3), h = Math.floor(m / 60), d = Math.floor(h / 24);
      return d > 0 ? d + 'd' : h > 0 ? h + 'h' : m + 'm';
    }

    // The three questions a person actually has to act on. Each is a filter and
    // a command; the header is those three counts, nothing descriptive.
    const INC_QUEUES = [
      { id: 'burning',      label: 'burning now',  color: 'var(--red)',
        why: 'critical or high, opened in the last 24h, still open',
        flag: '--severity high,critical --age <24h',
        test: i => i.status !== 'resolved' && ['high','critical'].includes(i.severity) && (Date.now() - incWhen(i)) < 86400e3 },
      { id: 'unverifiable', label: 'unverifiable', color: 'var(--amber)',
        why: 'open with no detection command - nobody can tell whether it is still happening',
        flag: '--open --no-detection',
        test: i => i.status !== 'resolved' && !(i.detection || '').trim() },
      { id: 'abandoned',    label: 'abandoned',    color: 'var(--text)',
        why: 'open, nothing changed for more than 7 days - resolve or drop',
        flag: '--open --stale 7d',
        test: i => i.status !== 'resolved' && (Date.now() - new Date(i.updated_at || 0).getTime()) > 7 * 86400e3 },
    ];
    function incSelectQueue(id) {
      App.incidentQueue = App.incidentQueue === id ? null : id;
      App.incidentCursor = 0;
      renderIncidents();
    }

    function incFiltered() {
      let list = [...App.incidents];
      if (App.incidentQueue) { const q = INC_QUEUES.find(q => q.id === App.incidentQueue); if (q) list = list.filter(q.test); }
      if (App.incidentStatusFilter !== 'all') list = list.filter(i => i.status === App.incidentStatusFilter);
      else list = list.filter(i => i.status !== 'resolved');
      if (App.incidentSeverityFilter !== 'all') list = list.filter(i => (i.severity || 'medium').toLowerCase() === App.incidentSeverityFilter);
      const rank = { critical: 0, high: 1, medium: 2, low: 3 };
      list.sort((a, b) => (rank[a.severity] ?? 2) - (rank[b.severity] ?? 2) || incWhen(b) - incWhen(a));
      return list;
    }

    function incClearCell() { App.incidentQueue = null; App.incidentCursor = 0; renderIncidents(); }

    function renderIncidents(keepSelection) {
      const open = App.incidents.filter(i => i.status !== 'resolved');
      const crit = open.filter(i => i.severity === 'critical').length;
      const high = open.filter(i => i.severity === 'high').length;
      const oldest = open.slice().sort((a, b) => incWhen(a) - incWhen(b))[0];

      // ---- summary
      document.getElementById('incTop').innerHTML = `
        <b>${open.length}</b><span>open</span>
        <span style="color:var(--text-muted)">&middot;</span>
        <span><span class="n" style="color:var(--red)">${crit}</span> critical</span>
        <span><span class="n" style="color:var(--amber)">${high}</span> high</span>
        ${oldest ? `<span style="color:var(--text-muted)">oldest open: ${incAgeLabel(oldest)} &middot; <span class="mono">${escapeHtml(oldest.service || 'infrastructure')}</span></span>` : ''}
        <span style="color:var(--text-muted)">30d: <span style="color:var(--red)">+${App.incidents.filter(i => Date.now() - incWhen(i) < 30 * 86400e3).length}</span> / <span style="color:var(--green)">&minus;${App.incidents.filter(i => i.resolved_at && Date.now() - new Date(i.resolved_at).getTime() < 30 * 86400e3).length}</span></span>
        <span class="inc-chips">
          ${['all','open','mitigated','resolved'].map(st => `<button class="chip ${App.incidentStatusFilter === st ? 'active' : ''}" onclick="filterIncidents('${st}')">${st === 'all' ? 'open + mitigated' : st}</button>`).join('')}
          <span class="sep"></span>
          ${['all','critical','high','medium','low'].map(sv => `<button class="chip ${App.incidentSeverityFilter === sv ? 'active' : ''}" onclick="filterIncidentsSeverity('${sv}')">${sv === 'all' ? 'any severity' : sv}</button>`).join('')}
        </span>`;

      // ---- attention queue
      document.getElementById('incQueue').innerHTML = INC_QUEUES.map(q => {
        const n = App.incidents.filter(q.test).length;
        return `<div class="b ${App.incidentQueue === q.id ? 'on' : ''} ${n ? '' : 'zero'}" onclick="incSelectQueue('${q.id}')">
          <div class="h"><b style="color:${n ? q.color : ''}">${n}</b><span>${q.label}</span></div>
          <div class="w">${q.why}</div>
          <div class="k">${escapeHtml(q.flag)}</div></div>`;
      }).join('');

      // ---- command line: the filter, made literal
      const list = incFiltered();
      const parts = ['incidents'];
      if (App.incidentStatusFilter !== 'all') parts.push('<i>--status</i> ' + App.incidentStatusFilter);
      if (App.incidentSeverityFilter !== 'all') parts.push('<i>--severity</i> ' + App.incidentSeverityFilter);
      if (App.incidentQueue) parts.push(INC_QUEUES.find(q => q.id === App.incidentQueue).flag.replace(/--(\S+)/g, '<i>--$1</i>'));
      document.getElementById('incCmd').innerHTML =
        `<span class="p">kyb</span><span>${parts.join(' ')}</span><span class="cur"></span>
         <span class="cnt">${list.length} of ${App.incidents.length}${App.incidentStatusFilter === 'all' ? ' &middot; open only' : ''}</span>`;

      // ---- rows + peek under the cursor
      // A background refresh may reorder the list; the selection is an incident, not a row number.
      if (keepSelection && App.incidentCursorKey) { const at = list.findIndex(i => i.key === App.incidentCursorKey); if (at >= 0) App.incidentCursor = at; }
      if (App.incidentCursor == null || App.incidentCursor >= list.length) App.incidentCursor = 0;
      App.incidentCursorKey = list.length ? list[App.incidentCursor].key : null;
      const rows = document.getElementById('incidentRows');
      if (!list.length) { rows.innerHTML = `<div class="inc-empty">Nothing matches. <kbd>esc</kbd> clears the queue filter.</div>`; return; }
      rows.innerHTML = list.map((inc, idx) => {
        const cur = idx === App.incidentCursor;
        const sev = (inc.severity || 'medium').toLowerCase();
        let peek = '';
        if (cur) {
          const hist = (App.detailCache.get(inc.key) || {}).history;
          peek = `<div class="inc-peek">
            ${inc.detection ? `<div class="k">detection &middot; is it still happening?</div><code>${escapeHtml(inc.detection)}</code>`
                              : `<div class="k" style="color:var(--amber)">no detection recorded &middot; a report that cannot say whether it is still happening is a story</div>`}
            <div class="k">${inc.status} &middot; ${escapeHtml(inc.service || 'infrastructure')}${(inc.hosts || []).length ? ' &middot; ' + escapeHtml(inc.hosts.join(', ')) : ''} &middot; detected ${timeAgo(inc.detected_at || inc.started_at)}${inc.open_followups ? ` &middot; ${inc.open_followups} open follow-up${inc.open_followups > 1 ? 's' : ''}` : ''}</div>
            ${inc.resolution ? `<div><span style="color:var(--green)">Resolution:</span> ${escapeHtml(inc.resolution)}</div>` : ''}
            <div class="row">
              <a href="#/knowledge/${encodeURIComponent(inc.key)}" class="btn sm primary" onclick="handleCardClick(event, ${jsArg(inc.key)}, 'knowledge')">&#9166; Full report</a>
              ${inc.status !== 'resolved' ? `<button class="btn sm" onclick="event.stopPropagation(); openResolveIncidentModal(${jsArg(inc.key)}, ${jsArg(inc.title)})">r Resolve</button>` : ''}
              <button class="btn sm" onclick="event.stopPropagation(); openDiffForEntry(${jsArg(inc.key)})">d Diff</button>
              <button class="btn sm" onclick="event.stopPropagation(); openHistoryForEntry(${jsArg(inc.key)})">h History${hist ? ` (${hist.length})` : ''}</button>
              <button class="btn sm" onclick="event.stopPropagation(); openEditModal(${jsArg(inc.key)})">e Edit</button>
              <a href="#/graph/${encodeURIComponent(inc.key)}" class="btn sm" onclick="navigateToGraphNode(event, ${jsArg(inc.key)})">Topology</a>
            </div></div>`;
        }
        // What makes the row actionable sits in the row; under the cursor the peek already says it.
        const det = (inc.detection || '').trim();
        const note = cur ? ''
          : inc.status === 'resolved' ? (inc.resolution ? `<div class="inc-note res"><b>Resolved</b> &middot; ${escapeHtml(inc.resolution)}</div>` : '')
          : det ? `<div class="inc-note det"><code title="${escapeHtml(det)}">${escapeHtml(det)}</code></div>`
          : `<div class="inc-note"><b>No detection</b> &middot; nobody can tell whether it is still happening</div>`;
        return `<div class="inc-r ${cur ? 'cur' : ''} ${inc.status}" data-idx="${idx}" onclick="App.incidentCursor=${idx}; renderIncidents();" ondblclick="handleCardClick(event, ${jsArg(inc.key)}, 'knowledge')">
            <span class="n">${idx + 1}</span>
            <span class="dot ${inc.status === 'resolved' ? 'resolved' : sev}"></span>
            <div class="t"><b>${escapeHtml(inc.title)}</b><span>${escapeHtml(inc.key)}</span></div>
            <span class="sv">${escapeHtml(inc.service || 'infrastructure')}</span>
            <span class="age">${incAgeLabel(inc)}</span>
            ${note}
          </div>${peek}`;
      }).join('');
      const curEl = rows.querySelector('.inc-r.cur');
      if (curEl && App.incidentKeyNav) { curEl.scrollIntoViewIfNeeded ? curEl.scrollIntoViewIfNeeded(false) : null; App.incidentKeyNav = false; }
    }

    // Keyboard, incidents tab only. Registered in the capture phase so it runs
    // before the global handler and can claim r/d/h/Enter/Esc for the cursor row.
    window.addEventListener('keydown', (e) => {
      if (App.activeTab !== 'incidents') return;
      if (['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName)) return;
      if (document.querySelector('.modal-backdrop.open')) return;
      const list = incFiltered();
      const cur = list[App.incidentCursor || 0];
      const claim = () => { e.preventDefault(); e.stopImmediatePropagation(); };
      if (e.key === 'j' || e.key === 'ArrowDown') { claim(); App.incidentCursor = Math.min((App.incidentCursor || 0) + 1, list.length - 1); App.incidentKeyNav = true; renderIncidents(); }
      else if (e.key === 'k' || e.key === 'ArrowUp') { claim(); App.incidentCursor = Math.max((App.incidentCursor || 0) - 1, 0); App.incidentKeyNav = true; renderIncidents(); }
      else if (e.key === 'Escape' && App.incidentQueue) { claim(); incClearCell(); }
      else if (!cur) return;
      else if (e.key === 'Enter') { claim(); location.hash = '#/knowledge/' + encodeURIComponent(cur.key); }
      else if (e.key === 'r' && cur.status !== 'resolved') { claim(); openResolveIncidentModal(cur.key, cur.title); }
      else if (e.key === 'd') { claim(); openDiffForEntry(cur.key); }
      else if (e.key === 'h') { claim(); openHistoryForEntry(cur.key); }
      else if (e.key === 'e') { claim(); openEditModal(cur.key); }
    }, true);

    function filterIncidents(status) {
      App.incidentStatusFilter = status;
      renderIncidents();
    }

    function filterIncidentsSeverity(sev) {
      App.incidentSeverityFilter = sev;
      ['All', 'Critical', 'High', 'Medium', 'Low'].forEach(name => {
        const btn = document.getElementById(`sevChip${name}`);
        if (btn) btn.classList.toggle('active', name.toLowerCase() === sev);
      });
      renderIncidents();
    }

    function openResolveIncidentModal(key, title) {
      App.activeResolveKey = key;
      document.getElementById('resolveIncidentTargetText').textContent = `Incident: ${title} (${key})`;
      document.getElementById('resolveIncidentOutcome').value = '';
      openModal('resolveIncidentModal');
    }

    async function submitResolveIncident() {
      const outcome = document.getElementById('resolveIncidentOutcome').value.trim();
      const author = document.getElementById('resolveIncidentAuthor').value.trim() || 'Alexander Panasenko <alex@prod.codes>';
      if (!outcome) {
        showToast('Please provide a resolution outcome summary', 'error');
        return;
      }

      const res = await apiPost(`/incidents/${encodeURIComponent(App.activeResolveKey)}/resolve`, {
        resolution: outcome
      });

      if (res.ok) {
        closeModal('resolveIncidentModal');
        showToast(`Incident ${App.activeResolveKey} resolved!`, 'success');
        App.detailCache.delete(App.activeResolveKey);
        await refreshAll();
        if (App.selectedKey === App.activeResolveKey) {
          selectEntry(App.activeResolveKey, false);
        }
      } else {
        showToast(`Failed: ${res.data?.error || res.error || 'Server error'}`, 'error');
      }
    }

