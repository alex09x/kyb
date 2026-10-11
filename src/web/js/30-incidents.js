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
      const when = incWhen(i);
      if (!when) return '';  // no date at all is not "since 1970"
      const ms = Date.now() - when;
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
    const INC_PAGE = 40;
    function incSelectQueue(id) {
      App.incidentQueue = App.incidentQueue === id ? null : id;
      App.incidentShown = INC_PAGE;
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

    function incClearCell() { App.incidentQueue = null; App.incidentCursor = 0; App.incidentShown = INC_PAGE; renderIncidents(); }

    function renderIncidents(keepSelection) {
      const open = App.incidents.filter(i => i.status !== 'resolved');
      const crit = open.filter(i => i.severity === 'critical').length;
      const high = open.filter(i => i.severity === 'high').length;
      const oldest = open.slice().sort((a, b) => incWhen(a) - incWhen(b))[0];

      // ---- the filter bar: status and severity as segments, what is left, the one action
      const list = incFiltered();
      const q = App.incidentQueue && INC_QUEUES.find(x => x.id === App.incidentQueue);
      document.getElementById('incTop').innerHTML = `
        <span class="segs">${['all','open','mitigated','resolved'].map(st => `<button class="chip ${App.incidentStatusFilter === st ? 'active' : ''}" onclick="filterIncidents('${st}')">${st === 'all' ? 'open + mitigated' : st}</button>`).join('')}</span>
        <span class="segs">${['all','critical','high','medium','low'].map(sv => `<button class="chip ${App.incidentSeverityFilter === sv ? 'active' : ''}" onclick="filterIncidentsSeverity('${sv}')">${sv === 'all' ? 'any severity' : sv}</button>`).join('')}</span>
        <span class="bar-r">
          <span class="cnt"><span id="incCount">${list.length} of ${App.incidents.length}${App.incidentStatusFilter === 'all' ? ' · open and mitigated' : ''}</span>${q ? ` · ${q.label} · <a href="javascript:void(0)" onclick="incClearCell()">clear</a>` : ''}</span>
          <button class="btn sm" data-write onclick="openNewModal('incident')">Report incident</button>
        </span>`;

      // ---- attention queue
      document.getElementById('incQueue').innerHTML = INC_QUEUES.map(q => {
        const n = App.incidents.filter(q.test).length;
        return `<button type="button" class="b ${App.incidentQueue === q.id ? 'on' : ''} ${n ? '' : 'zero'}" aria-pressed="${App.incidentQueue === q.id}" onclick="incSelectQueue('${q.id}')">
          <div class="h"><b style="color:${n ? q.color : ''}">${n}</b><span>${q.label}</span></div>
          <div class="w">${q.why}</div></button>`;
      }).join('');


      // ---- rows + peek under the cursor
      // A background refresh may reorder the list; the selection is an incident, not a row number.
      if (keepSelection && App.incidentCursorKey) { const at = list.findIndex(i => i.key === App.incidentCursorKey); if (at >= 0) App.incidentCursor = at; }
      if (App.incidentCursor == null || App.incidentCursor >= list.length) App.incidentCursor = 0;
      App.incidentCursorKey = list.length ? list[App.incidentCursor].key : null;
      const rows = document.getElementById('incidentRows');
      if (!list.length) { rows.innerHTML = `<div class="inc-empty">Nothing matches. <kbd>esc</kbd> clears the queue filter.</div>`; return; }
      // A long list is paged: the first screenfuls draw at once, the rest on request.
      // The cursor keys walk the whole list; landing past the page extends it.
      App.incidentShown = Math.max(App.incidentShown || INC_PAGE, App.incidentCursor + 1);
      const shown = list.slice(0, App.incidentShown);
      rows.innerHTML = shown.map((inc, idx) => {
        const cur = idx === App.incidentCursor;
        const sev = (inc.severity || 'medium').toLowerCase();
        let peek = '';
        if (cur) {
          const hist = (App.detailCache.get(inc.key) || {}).history;
          const K = jsArg(inc.key), svc = (inc.service || 'infrastructure').toLowerCase();
          peek = `<div class="inc-peek">
            ${inc.detection ? `<div class="detect"><span><span class="l">Is it still happening? </span><code>${escapeHtml(inc.detection)}</code></span><button class="btn sm" onclick="event.stopPropagation(); copyToClipboard(${jsArg(inc.detection)}, 'Check command copied!')">Copy</button></div>`
              : inc.status !== 'resolved' ? `<div class="callout"><b>no detection</b><span>Add a command that shows whether this is still happening - the report is a story until then.</span><button class="btn sm" data-write onclick="event.stopPropagation(); openEditModal(${K})">Add check</button></div>` : ''}
            <div class="k">${inc.status} &middot; ${escapeHtml(inc.service || 'infrastructure')}${(inc.hosts || []).length ? ' &middot; ' + escapeHtml(inc.hosts.join(', ')) : ''} &middot; detected ${timeAgo(inc.detected_at || inc.started_at)}${inc.open_followups ? ` &middot; ${inc.open_followups} open follow-up${inc.open_followups > 1 ? 's' : ''}` : ''}</div>
            ${inc.resolution ? `<div><span style="color:var(--green)">Resolution:</span> ${escapeHtml(inc.resolution)}</div>` : ''}
            <div class="row">
              <a href="#/knowledge/${encodeURIComponent(inc.key)}" class="btn sm primary" onclick="handleCardClick(event, ${K}, 'knowledge')">Open report <kbd>&#9166;</kbd></a>
              ${inc.status !== 'resolved' ? `<button class="btn sm success" data-write onclick="event.stopPropagation(); openResolveIncidentModal(${K}, ${jsArg(inc.title)})">Resolve <kbd>r</kbd></button>` : ''}
              <button class="btn sm" data-write onclick="event.stopPropagation(); openEditModal(${K})">Edit <kbd>e</kbd></button>
              <button class="btn sm ghost" onclick="event.stopPropagation(); openHistoryForEntry(${K})">Versions${hist ? ` · ${hist.length}` : ''} <kbd>h</kbd></button>
              <button class="btn sm ghost" onclick="event.stopPropagation(); openDiffForEntry(${K})">Diff <kbd>d</kbd></button>
              <a href="${placeHash({kind: 'service', name: svc})}" class="btn sm ghost" onclick="openPlace(event, 'service', ${jsArg(svc)})">Service</a>
            </div></div>`;
        }
        // What makes the row actionable sits in the row; under the cursor the peek already says it.
        const det = (inc.detection || '').trim();
        const note = cur ? ''
          : inc.status === 'resolved' ? (inc.resolution ? `<div class="inc-note res"><b>Resolved</b> &middot; ${escapeHtml(inc.resolution)}</div>` : '')
          : det ? `<div class="inc-note det"><code title="${escapeHtml(det)}">${escapeHtml(det)}</code></div>`
          : `<div class="inc-note"><b>no detection</b><span class="why"> &middot; nobody can tell whether it is still happening</span></div>`;  // the queue header above says why that matters; the row only has to show it
        return `<div class="inc-r ${cur ? 'cur' : ''} ${inc.status}" data-idx="${idx}" onclick="App.incidentCursor=${idx}; renderIncidents();" ondblclick="handleCardClick(event, ${jsArg(inc.key)}, 'knowledge')">
            <span class="n">${idx + 1}</span>
            <span class="dot ${inc.status === 'resolved' ? 'resolved' : sev}"></span>
            <div class="t"><b>${escapeHtml(inc.title)}</b><span>${escapeHtml(inc.key)}</span></div>
            <span class="sv">${escapeHtml(inc.service || 'infrastructure')}</span>
            <span class="age">${incAgeLabel(inc)}</span>
            ${note}
          </div>${peek}`;
      }).join('') + (list.length > shown.length
        ? `<div class="inc-more" onclick="App.incidentShown += ${INC_PAGE}; renderIncidents();">${list.length - shown.length} more &middot; <span class="acc">show ${Math.min(INC_PAGE, list.length - shown.length)}</span></div>` : '');
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
      App.incidentShown = INC_PAGE;
      App.incidentCursor = 0;  // a new filter is a new list; the cursor starts at its top
      renderIncidents();
    }

    function filterIncidentsSeverity(sev) {
      App.incidentSeverityFilter = sev;
      App.incidentShown = INC_PAGE;
      App.incidentCursor = 0;  // a new filter is a new list; the cursor starts at its top
      renderIncidents();
    }

    function openResolveIncidentModal(key, title) {
      if (refuseReadOnly()) return;
      App.activeResolveKey = key;
      document.getElementById('resolveIncidentTitle').textContent = `Resolve: ${title || key}`;
      document.getElementById('resolveIncidentTargetText').textContent = key;
      document.getElementById('resolveIncidentOutcome').value = '';
      openModal('resolveIncidentModal');
    }

    async function submitResolveIncident() {
      if (!markRequired(['resolveIncidentOutcome'])) {
        showToast('Say what fixed it', 'error');
        return;
      }
      const outcome = document.getElementById('resolveIncidentOutcome').value.trim();

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

