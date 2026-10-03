    // Filter & Sort Knowledge List
    function getFilteredEntries() {
      let list = [...App.entries];
      if (App.currentFilter === 'knowledge') list = list.filter(e => e.kind === 'knowledge');
      else if (App.currentFilter === 'incident') list = list.filter(e => e.kind === 'incident');
      else if (App.currentFilter === 'task') list = list.filter(e => e.kind === 'task');

      if (App.tagFilter) {
        const want = App.tagFilter.toLowerCase();
        list = list.filter(e => (e.tags || []).some(t => String(t).toLowerCase() === want));
      }
      if (!App.isBackendSearchResults && App.searchQuery.trim()) {
        const q = App.searchQuery.toLowerCase();
        list = list.filter(e =>
          (e.title && e.title.toLowerCase().includes(q)) ||
          (e.key && e.key.toLowerCase().includes(q)) ||
          (e.tags && e.tags.some(t => t.toLowerCase().includes(q))) ||
          (e.body && e.body.toLowerCase().includes(q))
        );
      }

      const sortMode = document.getElementById('sortSelect')?.value || 'newest';
      if (sortMode === 'newest') {
        list.sort((a, b) => new Date(b.updated_at || b.committed_at || 0) - new Date(a.updated_at || a.committed_at || 0));
      } else if (sortMode === 'oldest') {
        list.sort((a, b) => new Date(a.updated_at || a.committed_at || 0) - new Date(b.updated_at || b.committed_at || 0));
      } else if (sortMode === 'az') {
        list.sort((a, b) => (a.title || a.key).localeCompare(b.title || b.key));
      }

      return list;
    }

    // In-memory instant detail cache. A write bumps the generation and clears
    // it; a read that started before the write must not put its stale answer back.
    App.detailCache = new Map();
    App.cacheGen = 0;

    // Hovering warms the document only: history walks git on the server and a
    // mouse moving down a list must not fire fifty of those.
    App.entryPrefetch = App.entryPrefetch || new Map();  // key -> entry
    async function prefetchEntry(key) {
      if (!key || App.detailCache.has(key) || App.entryPrefetch.has(key)) return;
      const gen = App.cacheGen;
      try {
        const entryData = await apiFetch(`/knowledge/${encodeURIComponent(key)}`);
        if (entryData && gen === App.cacheGen) App.entryPrefetch.set(key, entryData);
      } catch (_) {}
    }

    function highlightText(text, query) {
      if (!text || !query || !query.trim()) return escapeHtml(text);
      const safe = escapeHtml(text);
      const q = escapeHtml(query.trim());
      const regex = new RegExp(`(${q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')})`, 'gi');
      return safe.replace(regex, '<mark class="search-highlight">$1</mark>');
    }

    function renderKnowledgeList() {
      const list = getFilteredEntries();
      const container = document.getElementById('knowledgeList');
      document.getElementById('listCountLabel').textContent = `${list.length} item${list.length === 1 ? '' : 's'}`;

      if (!list.length) {
        container.innerHTML = `<div style="padding: 30px; text-align:center; color: var(--text-muted);">No entries matching criteria</div>`;
        return;
      }

      container.innerHTML = list.map(item => {
        const isSel = item.key === App.selectedKey;
        const kindBadge = item.kind === 'incident'
          ? `<span class="badge badge-${item.severity || 'incident'}">${item.severity ? escapeHtml(item.severity) : 'incident'}</span>`
          : item.kind === 'task'
            ? `<span class="badge badge-task">task</span>`
            : '';

        const cached = App.detailCache.get(item.key);
        const revCount = (cached && cached.history && cached.history.length) || 0;

        return `
          <a href="#/knowledge/${encodeURIComponent(item.key)}" 
             class="item-card ${isSel ? 'selected' : ''}" 
             data-key="${escapeHtml(item.key)}" 
             onmouseenter="prefetchEntry(${jsArg(item.key)})" 
             onclick="handleCardClick(event, ${jsArg(item.key)}, 'knowledge')">
            <div class="card-top">
              <span class="item-title">${highlightText(item.title || item.key, App.searchQuery)}</span>
              ${kindBadge}
            </div>
            <div class="item-key">${highlightText(item.key, App.searchQuery)}</div>
            <div class="item-meta" style="margin-top: 8px; display:flex; justify-content:space-between; align-items:center;">
              <div>
                <span>${timeAgo(item.updated_at || item.committed_at)}</span>
                ${(item.tags || []).slice(0, 3).map(t => `<span class="badge-tag">#${highlightText(t, App.searchQuery)}</span>`).join('')}
                ${revCount > 1 ? `<span class="rev-badge" title="${revCount} revisions recorded">${revCount} revs</span>` : ''}
              </div>
            </div>
          </a>
        `;
      }).join('');

      // Auto-select first item ONLY if on search tab and none is selected
      if (App.activeTab === 'search' && !App.selectedKey && list.length > 0) {
        selectEntry(list[0].key, false);
      }
    }

    async function selectEntry(key, updateHash = true) {
      if (!key) return;
      App.selectedKey = key;
      if (updateHash) {
        const targetHash = `#/knowledge/${encodeURIComponent(key)}`;
        if (window.location.hash !== targetHash) {
          history.pushState(null, '', targetHash);
        }
      }
      document.title = `KYB — ${key}`;

      document.querySelectorAll('.item-card').forEach(c => {
        c.classList.toggle('selected', c.getAttribute('data-key') === key);
      });

      const detailEl = document.getElementById('detailView');

      // 1. Instant Cache Hit (0ms!)
      const cached = App.detailCache.get(key);
      if (cached) {
        renderDetailView(cached.entry, cached.history);
        return;
      }

      // 2. Instant optimistic skeleton from App.entries (0ms)
      const known = App.entries.find(e => e.key === key) || App.tasks.find(t => t.key === key) || App.incidents.find(i => i.key === key);
      if (known) {
        renderDetailView({ ...known, body: known.body || 'Loading document body from fleet memory...' }, [], true);
      } else {
        detailEl.innerHTML = `<div style="padding: 40px; text-align:center; color: var(--text-muted);">Loading ${key}...</div>`;
      }

      // 3. Network fetch in background. The entry is quick; its history walks
      // git and is not, so the document is drawn the moment it arrives and the
      // version count fills in after.
      const gen = App.cacheGen;
      const historyP = apiFetch(`/knowledge/${encodeURIComponent(key)}/history`);
      const entryData = App.entryPrefetch.get(key) || await apiFetch(`/knowledge/${encodeURIComponent(key)}`);

      if (!entryData) {
        if (!known) detailEl.innerHTML = `<div class="detail-empty">Error loading entry ${key}</div>`;
        return;
      }
      // Someone else was selected while this was in flight: the answer is nobody's now.
      if (App.selectedKey !== key) return;
      // A write happened while this was in flight: the answer may be stale, so ask again rather than draw it.
      if (gen !== App.cacheGen) return selectEntry(key, false);
      renderDetailView(entryData, [], true);

      const historyData = await historyP;
      const historyList = (historyData && (historyData.versions || historyData.history)) ? (historyData.versions || historyData.history) : [];
      if (App.selectedKey !== key) return;
      if (gen !== App.cacheGen) return selectEntry(key, false);
      App.detailCache.set(key, { entry: entryData, history: historyList });
      renderDetailView(entryData, historyList);
    }

    function renderEntityHeroBanner(e, historyList = []) {
      const createdVer = (historyList && historyList.length) ? historyList[historyList.length - 1] : null;
      const lastVer = (historyList && historyList.length) ? historyList[0] : null;
      const creator = (createdVer && createdVer.author) || e.author || e.assignee || 'Alexander Panasenko <alex@prod.codes>';
      const createdAt = (createdVer && createdVer.committed_at) || e.created_at || e.detected_at || e.committed_at || e.updated_at;

      if (e.kind === 'task') {
        const isDone = e.status === 'done' || e.status === 'closed' || e.status === 'dropped' || e.status === 'resolved' || e.archived;
        const isBlocked = e.status === 'blocked';
        const isProgress = e.status === 'in_progress';
        const isOpen = !isDone && !isBlocked && !isProgress;

        return `
          <div class="entity-hero-card task-hero-panel">
            <div class="hero-top-bar">
              <div class="hero-identity">
                <span class="badge badge-task">TASK</span>
                <span class="hero-key">${escapeHtml(e.key)}</span>
                <span class="hero-attribution">
                  <span>Created by <strong style="color:var(--text);">${escapeHtml(creator)}</strong></span>
                  <span>•</span>
                  <span>${createdAt ? timeAgo(createdAt) : 'recent'}</span>
                </span>
                ${e.parent_task ? `
                  <a href="#/knowledge/${encodeURIComponent(e.parent_task)}" class="badge-tag" onclick="handleCardClick(event, ${jsArg(e.parent_task)}, 'knowledge')">
                    ↳ Part of ${escapeHtml(e.parent_task)}
                  </a>
                ` : ''}
              </div>

              <div style="display:flex; gap:6px; align-items:center; flex-wrap:wrap;">
                ${isOpen ? `
                  <button class="btn sm primary" onclick="transitionTask(${jsArg(e.key)}, 'in_progress')">Start Progress</button>
                  <button class="btn sm warning" onclick="promptBlockTask(${jsArg(e.key)}, ${jsArg(e.title)})">Block Task</button>
                  <button class="btn sm success" onclick="openResolveTaskModal(${jsArg(e.key)}, ${jsArg(e.title)})">Resolve Task</button>
                ` : ''}
                ${isProgress ? `
                  <button class="btn sm warning" onclick="promptBlockTask(${jsArg(e.key)}, ${jsArg(e.title)})">Block Task</button>
                  <button class="btn sm success" onclick="openResolveTaskModal(${jsArg(e.key)}, ${jsArg(e.title)})">Resolve Task</button>
                  <button class="btn sm" onclick="transitionTask(${jsArg(e.key)}, 'open')">Move to Backlog</button>
                ` : ''}
                ${isBlocked ? `
                  <button class="btn sm primary" onclick="transitionTask(${jsArg(e.key)}, 'in_progress')">Unblock & Resume</button>
                  <button class="btn sm success" onclick="openResolveTaskModal(${jsArg(e.key)}, ${jsArg(e.title)})">Resolve Task</button>
                  <button class="btn sm" onclick="transitionTask(${jsArg(e.key)}, 'open')">Move to Backlog</button>
                ` : ''}
                ${isDone ? `
                  <button class="btn sm primary" onclick="transitionTask(${jsArg(e.key)}, 'in_progress')">Re-open Task</button>
                ` : ''}
              </div>
            </div>

            <!-- Lifecycle Stepper -->
            <div class="lifecycle-stepper">
              <div class="step-item ${isOpen ? 'active' : (isProgress || isDone ? 'completed' : '')}" onclick="transitionTask(${jsArg(e.key)}, 'open')">
                <div class="step-indicator">${isOpen ? '●' : (isProgress || isDone ? '✓' : '1')}</div>
                <span>1. Backlog</span>
              </div>
              <div class="step-connector ${isProgress || isDone ? 'active' : ''}"></div>
              <div class="step-item ${isProgress ? 'active' : (isDone ? 'completed' : '')}" onclick="transitionTask(${jsArg(e.key)}, 'in_progress')">
                <div class="step-indicator">${isProgress ? '●' : (isDone ? '✓' : '2')}</div>
                <span>2. In Progress</span>
              </div>
              <div class="step-connector ${isDone ? 'active' : ''}"></div>
              <div class="step-item ${isDone ? 'active completed' : ''}" onclick="openResolveTaskModal(${jsArg(e.key)}, ${jsArg(e.title)})">
                <div class="step-indicator">${isDone ? '✓' : '3'}</div>
                <span>3. Done / Resolved</span>
              </div>
              ${isBlocked ? `
                <div style="margin-left:auto; display:flex; align-items:center; gap:6px;">
                  <span class="badge badge-blocked" style="font-size:11.5px; font-weight:700; padding:3px 8px;">CURRENTLY BLOCKED</span>
                </div>
              ` : ''}
            </div>

            <!-- Executive Meta Grid -->
            <div class="hero-meta-grid">
              <div class="meta-cell">
                <div class="meta-cell-label">Current Status</div>
                <div class="meta-cell-value">
                  <span class="badge badge-${(e.status || 'open').toLowerCase()}" style="font-size:12px; font-weight:700;">
                    ${(e.status || 'open').toUpperCase()}
                  </span>
                </div>
              </div>
              <div class="meta-cell">
                <div class="meta-cell-label">Priority Level</div>
                <div class="meta-cell-value">
                  <span class="badge badge-${(e.priority || 'medium').toLowerCase()}" style="font-size:12px; font-weight:600;">
                    ${(e.priority || 'normal').toUpperCase()}
                  </span>
                </div>
              </div>
              <div class="meta-cell">
                <div class="meta-cell-label">Assignee</div>
                <div class="meta-cell-value" style="font-weight:600;">
                  <span>${escapeHtml(e.assignee || 'Unassigned')}</span>
                </div>
              </div>
              <div class="meta-cell">
                <div class="meta-cell-label">Lifecycle Stage</div>
                <div class="meta-cell-value" style="color:var(--text-dim); font-size:12px;">
                  ${isDone ? 'Archived in Canon' : isBlocked ? 'Blocked in execution' : isProgress ? 'Active development' : 'Sprint Backlog queue'}
                </div>
              </div>
            </div>

            ${e.blocked_reason ? `
              <div class="hero-callout hero-callout-warning">
                <div class="callout-header">
                  <strong>Blocked Reason:</strong>
                  <button class="btn sm primary" style="padding:2px 8px; font-size:11px;" onclick="transitionTask(${jsArg(e.key)}, 'in_progress')">Unblock</button>
                </div>
                <div>${escapeHtml(e.blocked_reason)}</div>
              </div>
            ` : ''}

            ${e.resolution ? `
              <div class="hero-callout hero-callout-success">
                <div class="callout-header">
                  <strong>Resolution & Verification Outcome:</strong>
                  ${e.resolved_at ? `<span style="font-size:11px; opacity:0.8;">Resolved: ${timeAgo(e.resolved_at)}</span>` : ''}
                </div>
                <div style="white-space:pre-wrap;">${escapeHtml(e.resolution)}</div>
              </div>
            ` : ''}

            <div class="hero-section-divider">
              <span>Task Acceptance Criteria & Work Specification</span>
            </div>
          </div>
        `;
      } else if (e.kind === 'incident') {
        const isResolved = e.status === 'resolved';
        return `
          <div class="entity-hero-card incident-hero-panel">
            <div class="hero-top-bar">
              <div class="hero-identity">
                <span class="badge badge-incident">INCIDENT</span>
                <span class="hero-key">${escapeHtml(e.key)}</span>
                <span class="hero-attribution">
                  <span>Reported by <strong style="color:var(--text);">${escapeHtml(creator)}</strong></span>
                  <span>•</span>
                  <span>${createdAt ? timeAgo(createdAt) : 'recent'}</span>
                </span>
              </div>

              <div style="display:flex; gap:6px; align-items:center; flex-wrap:wrap;">
                ${!isResolved ? `
                  <button class="btn sm success" onclick="openResolveIncidentModal(${jsArg(e.key)}, ${jsArg(e.title)})">Resolve Incident</button>
                ` : ''}
              </div>
            </div>

            <!-- Executive Incident Meta Grid -->
            <div class="hero-meta-grid">
              <div class="meta-cell">
                <div class="meta-cell-label">Incident Severity</div>
                <div class="meta-cell-value">
                  <span class="badge badge-${(e.severity || 'medium').toLowerCase()}" style="font-size:12px; font-weight:700;">
                    ${(e.severity || 'medium').toUpperCase()}
                  </span>
                </div>
              </div>
              <div class="meta-cell">
                <div class="meta-cell-label">Operational Status</div>
                <div class="meta-cell-value">
                  <span class="badge badge-${(e.status || 'open').toLowerCase()}" style="font-size:12px; font-weight:700;">
                    ${(e.status || 'open').toUpperCase()}
                  </span>
                </div>
              </div>
              <div class="meta-cell">
                <div class="meta-cell-label">Affected Service</div>
                <div class="meta-cell-value">
                  <span class="service-chip">${escapeHtml(e.service || 'infrastructure')}</span>
                </div>
              </div>
              <div class="meta-cell">
                <div class="meta-cell-label">Fleet Blast Radius</div>
                <div class="meta-cell-value">
                  ${(e.hosts && e.hosts.length) ? e.hosts.map(h => `<span class="host-pill">${escapeHtml(h)}</span>`).join(' ') : '<span style="color:var(--text-muted); font-size:12px;">All fleet nodes</span>'}
                </div>
              </div>
            </div>

            ${(e.knowledge && e.knowledge.length) ? `
              <div style="display:flex; align-items:center; gap:8px; margin: 10px 0; font-size:12px; flex-wrap:wrap;">
                <span style="font-size:11px; text-transform:uppercase; letter-spacing:0.5px; color:var(--text-muted); font-weight:700;">Linked Runbooks:</span>
                ${e.knowledge.map(k => `
                  <a href="#/knowledge/${encodeURIComponent(k)}" class="badge-tag" onclick="handleCardClick(event, ${jsArg(k)}, 'knowledge')">
                    ${escapeHtml(k)}
                  </a>
                `).join(' ')}
              </div>
            ` : ''}

            ${e.detection ? `
              <div class="block-box" style="margin-bottom:12px;">
                <div class="block-label" style="display:flex; justify-content:space-between; align-items:center;">
                  <span>Executable Detection Check / Blast Radius Verification:</span>
                  <button class="btn sm" style="padding:2px 6px; font-size:10.5px;" onclick="copyToClipboard(${jsArg(e.detection)}, 'Check command copied!')">Copy Command</button>
                </div>
                <code>${escapeHtml(e.detection)}</code>
              </div>
            ` : ''}

            ${e.resolution ? `
              <div class="hero-callout hero-callout-success">
                <div class="callout-header">
                  <strong>Resolution & Root Cause Post-Mortem:</strong>
                  ${e.resolved_at ? `<span style="font-size:11px; opacity:0.8;">Resolved: ${timeAgo(e.resolved_at)}</span>` : ''}
                </div>
                <div style="white-space:pre-wrap;">${escapeHtml(e.resolution)}</div>
              </div>
            ` : ''}

            <div class="hero-section-divider">
              <span>Incident Description, Impact & Diagnostic Log</span>
            </div>
          </div>
        `;
      } else {
        // Standard Knowledge Entry
        return `
          <div class="entity-hero-card" style="padding:12px 16px; margin-bottom:16px;">
            <div class="hero-top-bar" style="padding-bottom:8px; border-bottom:none;">
              <div class="hero-identity">
                <span class="badge badge-knowledge">KNOWLEDGE</span>
                <span class="hero-key">${escapeHtml(e.key)}</span>
                <span class="hero-attribution">
                  <span>Authored by <strong style="color:var(--text);">${escapeHtml(creator)}</strong></span>
                  <span>•</span>
                  <span>${createdAt ? timeAgo(createdAt) : 'recent'}</span>
                  ${lastVer && lastVer.author !== creator ? `
                    <span>• Last updated by <strong style="color:var(--text);">${escapeHtml(lastVer.author)}</strong></span>
                  ` : ''}
                </span>
              </div>
            </div>
            ${(e.refs && e.refs.length) ? `
              <div style="display:flex; align-items:center; gap:8px; margin-top:8px; font-size:12px; flex-wrap:wrap;">
                <span style="font-size:10.5px; text-transform:uppercase; letter-spacing:0.5px; color:var(--text-muted); font-weight:700;">Infrastructure Refs:</span>
                ${e.refs.map(r => `<span class="host-pill">${escapeHtml(r)}</span>`).join(' ')}
              </div>
            ` : ''}
          </div>
        `;
      }
    }

    function renderDetailView(e, historyList = [], isSkeleton = false) {
      const detailEl = document.getElementById('detailView');
      App.currentDetailBody = e.body || '';

      detailEl.innerHTML = `
        <div class="detail-header">
          <div>
            <div style="display:flex; align-items:center; gap:8px; margin-bottom:6px;">
              <span class="badge badge-${e.kind || 'knowledge'}">${e.kind || 'knowledge'}</span>
              <span style="font-family:var(--font-mono); font-size:12px; color:var(--accent); font-weight:700;">${e.key}</span>
              ${e.sha ? `<span style="font-family:var(--font-mono); font-size:11px; color:var(--text-muted);">${e.sha.slice(0,7)}</span>` : ''}
            </div>
            <h1 style="font-size:20px; font-weight:700; color:#fff;">${escapeHtml(e.title || e.key)}</h1>
            <div style="display:flex; gap:6px; margin-top:8px; flex-wrap:wrap;">
              ${(e.tags || []).map(t => `<a href="#/search?tag=${encodeURIComponent(t)}" class="badge-tag" style="text-decoration:none;" onclick="handleTagClick(event, ${jsArg(t)})">#${escapeHtml(t)}</a>`).join('')}
            </div>
            ${historyList.length > 1 ? `<div class="ver-line" onclick="switchDetailTab('history')">${historyList.length} versions &middot; last change ${timeAgo(historyList[0].committed_at)} &middot; <span>see what moved &rarr;</span></div>` : ''}
          </div>
          <div style="display:flex; gap:6px; flex-wrap:wrap;">
            <button class="btn sm" id="btnCopyDirectLink" onclick="copyDirectLink(${jsArg('#/knowledge/' + encodeURIComponent(e.key))})">Link</button>
            <a href="#/graph/${encodeURIComponent(e.key)}" class="btn sm" onclick="navigateToGraphNode(event, ${jsArg(e.key)})">Graph</a>
            <button class="btn sm primary" id="btnEditEntry" onclick="openEditModal(${jsArg(e.key)})">Edit</button>
            <button class="btn sm" onclick="copyToClipboard(${jsArg('kyb get ' + e.key)}, 'CLI command copied!')">CLI</button>
            <button class="btn sm" onclick="copyEntryBody(${jsArg(e.key)})">Markdown</button>
            <button class="btn sm" onclick="copyToClipboard(${jsArg(e.key)}, 'Key copied!')">Key</button>
          </div>
        </div>

        <div class="detail-tabs">
          <button class="detail-tab active" onclick="switchDetailTab('content')">Document</button>
          <button class="detail-tab" onclick="switchDetailTab('history')">Versions (${historyList.length})</button>
          <button class="detail-tab" onclick="switchDetailTab('raw')">Raw</button>
        </div>

        <div class="detail-body" id="detailBodyContent">
          ${renderEntityHeroBanner(e, historyList)}
          <div class="prose">${renderMarkdown(e.body)}</div>
        </div>

        <div class="detail-body" id="detailBodyHistory" style="display:none;">
          <div class="ver" id="verList"></div>
        </div>

        <div class="detail-body" id="detailBodyRaw" style="display:none;">
          <pre style="background:#06080c; border:1px solid var(--border); padding:16px; border-radius:6px; font-family:var(--font-mono); font-size:12px; color:var(--text); white-space:pre-wrap;">${escapeHtml(e.body)}</pre>
        </div>
      `;

      // The pane is rebuilt on every 4s refresh; the reader's tab must survive it.
      App.currentHistoryList = historyList;
      const sameKey = App.verKey === e.key;
      if (!sameKey) { App.detailTab = 'content'; App.verTextOpen = new Set(); }
      App.verKey = e.key;
      renderVersions();
      if (sameKey && App.detailTab && App.detailTab !== 'content') switchDetailTab(App.detailTab);
    }

