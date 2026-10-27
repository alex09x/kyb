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
        // the same pill as on the card: colour only for what demands it
        const sev = (item.severity || '').toLowerCase();
        const kindBadge = item.kind === 'incident'
          ? `<span class="pill ${sev === 'critical' ? 'crit' : sev === 'high' ? 'amber' : ''}">${sev ? escapeHtml(sev) : 'incident'}</span>`
          : item.kind === 'task'
            ? `<span class="pill">task</span>`
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
            <div class="item-meta">
              <span class="item-key">${highlightText(item.key, App.searchQuery)}</span>
              <span class="dot-sep">&middot;</span><span>${timeAgo(item.updated_at || item.committed_at)}</span>
              ${(item.tags || []).slice(0, 3).map(t => `<span class="item-tag">#${highlightText(t, App.searchQuery)}</span>`).join('')}
              ${revCount > 1 ? `<span class="item-tag" title="${revCount} revisions recorded">${revCount} versions</span>` : ''}
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

    // The state of an entry, read as one line: pills for what colour must say, words for the rest.
    // Actions that make sense in this state sit at the right. Anything that needs more than a
    // line - a blocked reason, a resolution, the detection check - is a callout below.
    // A card's state, in the places the design gives it: state pills beside the key, one line of
    // plain words under the title, the actions this state allows beside Edit, and whatever needs
    // more than a line (blocked reason, detection, resolution) as a callout at the top of the body.
    function entityParts(e, historyList = []) {
      const createdVer = (historyList && historyList.length) ? historyList[historyList.length - 1] : null;
      const lastVer = (historyList && historyList.length) ? historyList[0] : null;
      const creator = (createdVer && createdVer.author) || e.author || '';   // a commit made without a token names nobody
      const createdAt = (createdVer && createdVer.committed_at) || e.created_at || e.detected_at || e.committed_at || e.updated_at;
      const who = s => escapeHtml(String(s).replace(/\s*<[^>]*>/, '').trim() || s);
      const when = createdAt ? timeAgo(createdAt) : 'recent';
      const by = creator ? ` by ${who(creator)}` : '';
      const versions = historyList.length > 1 ? ` · ${historyList.length} versions, last change ${timeAgo(lastVer.committed_at)}` : '';
      const linked = (e.knowledge && e.knowledge.length)
        ? `<div class="dh-links"><span>Related:</span>${e.knowledge.map(k => `<a href="#/knowledge/${encodeURIComponent(k)}" class="badge-tag" onclick="handleCardClick(event, ${jsArg(k)}, 'knowledge')">${escapeHtml(k)}</a>`).join('')}</div>` : '';
      const resolution = e.resolution
        ? `<div class="callout good"><b>${e.kind === 'task' ? 'Done' : 'Resolved'}</b><span style="white-space:pre-wrap">${escapeHtml(e.resolution)}${e.resolved_at ? ` <span class="muted">· ${timeAgo(e.resolved_at)}</span>` : ''}</span></div>` : '';
      const K = jsArg(e.key), T = jsArg(e.title);

      if (e.kind === 'task') {
        const st = (e.status || 'open').toLowerCase();
        const isDone = ['done', 'closed', 'dropped', 'resolved'].includes(st) || e.archived;
        const isBlocked = st === 'blocked', isProgress = st === 'in_progress', isOpen = !isDone && !isBlocked && !isProgress;
        const pr = (e.priority || '').toLowerCase();
        const it = isDone ? null : dlItem(e.key);
        const start = label => `<button class="btn sm primary" data-write onclick="transitionTask(${K}, 'in_progress')">${label}</button>`;
        const block = `<button class="btn sm warning" data-write onclick="promptBlockTask(${K}, ${T})">Block</button>`;
        const done = `<button class="btn sm success" data-write onclick="openResolveTaskModal(${K}, ${T})">Done</button>`;
        const backlog = `<button class="btn sm" data-write onclick="transitionTask(${K}, 'open')">Backlog</button>`;
        const acts = isOpen ? start('Start') + block + done
          : isProgress ? block + done + backlog
          : isBlocked ? start('Unblock') + done + backlog
          : start('Reopen');
        return {
          hero: 'task-hero-panel',
          pills: `<span class="pill ${isBlocked ? 'amber' : isProgress ? 'acc' : isDone ? 'ok' : ''}">${st === 'in_progress' ? 'in progress' : escapeHtml(st)}</span>
                  ${pr ? `<span class="pill ${pr === 'critical' ? 'crit' : pr === 'high' ? 'amber' : ''}">${escapeHtml(pr)}</span>` : ''}`,
          words: `${escapeHtml(e.assignee || 'Unassigned')} · created ${when}${by}${e.parent_task ? ` · part of <a href="#/knowledge/${encodeURIComponent(e.parent_task)}" onclick="handleCardClick(event, ${jsArg(e.parent_task)}, 'knowledge')">${escapeHtml(e.parent_task)}</a>` : ''}${it ? ` · ${dueChip(it, true)}` : e.due ? ` · due ${dlDate(e.due)}` : ''}${versions}`,
          acts,
          body: `${it ? dlCallout(e, it) : ''}${e.blocked_reason ? `<div class="callout"><b>Blocked</b><span>${escapeHtml(e.blocked_reason)}</span></div>` : ''}${resolution}${linked}`,
        };
      }

      if (e.kind === 'incident') {
        const st = (e.status || 'open').toLowerCase(), sev = (e.severity || 'medium').toLowerCase();
        return {
          hero: 'incident-hero-panel',
          pills: `<span class="pill ${sev === 'critical' ? 'crit' : sev === 'high' ? 'amber' : ''}">${escapeHtml(sev)}</span>
                  <span class="pill ${st === 'resolved' ? 'ok' : st === 'mitigated' ? 'amber' : ''}">${escapeHtml(st)}</span>`,
          words: `<span class="mono">${escapeHtml(e.service || 'infrastructure')}</span>${(e.hosts && e.hosts.length) ? ` on <span class="mono">${e.hosts.map(escapeHtml).join(', ')}</span>` : ''} · opened ${when}${by}${versions}`,
          acts: st !== 'resolved' ? `<button class="btn sm success" data-write onclick="openResolveIncidentModal(${K}, ${T})">Resolve</button>` : '',
          body: `${e.detection
            ? `<div class="detect"><span><span class="l">Is it still happening? </span><code>${escapeHtml(e.detection)}</code></span><button class="btn sm" onclick="copyToClipboard(${jsArg(e.detection)}, 'Check command copied!')">Copy</button></div>`
            : (st !== 'resolved' ? `<div class="callout"><b>No detection</b><span>Nobody can tell whether this is still happening. Add a command and what a healthy answer looks like.</span><button class="btn sm" data-write onclick="openEditModal(${K})">Add check</button></div>` : '')}${resolution}${linked}`,
        };
      }

      const changed = lastVer && lastVer.author && lastVer.author !== creator ? ` · last changed by ${who(lastVer.author)}` : '';
      return {
        hero: '',
        pills: `<span class="pill">${escapeHtml(e.kind)}</span>`,
        words: `Written ${when}${by}${changed}${(e.refs && e.refs.length) ? ` · refs: ${e.refs.map(r => `<span class="mono">${escapeHtml(r)}</span>`).join(', ')}` : ''}${versions}`,
        acts: '',
        body: '',
      };
    }

    // The "More" menu closes on any click that is not its own toggle (the toggle stops propagation).
    document.addEventListener('click', () => document.querySelectorAll('.more.open').forEach(m => m.classList.remove('open')));

    function renderDetailView(e, historyList = [], isSkeleton = false) {
      const detailEl = document.getElementById('detailView');
      App.currentDetailBody = e.body || '';

      // a skeleton drawn from a list row may not carry its kind; the key's prefix says it
      if (!e.kind) e = { ...e, kind: e.key.startsWith('inc-') ? 'incident' : e.key.startsWith('task-') ? 'task' : 'knowledge' };
      const parts = entityParts(e, historyList);
      detailEl.innerHTML = `
        <div class="detail-header">
          <div class="dh-kind">
            <span class="dh-pills">${parts.pills}</span>
            <span class="mono dh-key">${escapeHtml(e.key)}</span>
          </div>
          <h1>${escapeHtml(e.title || e.key)}</h1>
          <div class="dh-meta"><span class="dh-words">${parts.words}</span>${(e.tags || []).map(t => `<a href="#/search?tag=${encodeURIComponent(t)}" class="dh-tag" onclick="handleTagClick(event, ${jsArg(t)})">#${escapeHtml(t)}</a>`).join('')}</div>
          <div class="dh-row">
            <div class="detail-tabs">
              <button class="detail-tab active" onclick="switchDetailTab('content')">${e.kind === 'incident' ? 'Report' : e.kind === 'task' ? 'Task' : 'Document'}</button>
              <button class="detail-tab" onclick="switchDetailTab('history')">Versions${historyList.length ? ` · ${historyList.length}` : ''}</button>
              <button class="detail-tab" onclick="switchDetailTab('raw')">Raw</button>
            </div>
            <div class="dh-acts">
              <span class="dh-state-acts ${parts.hero}">${parts.acts}</span>
              <button class="btn sm" id="btnEditEntry" data-write onclick="openEditModal(${jsArg(e.key)})">Edit <kbd>e</kbd></button>
              <div class="more">
                <button class="btn sm ghost" onclick="this.parentElement.classList.toggle('open'); event.stopPropagation();">More ▾</button>
                <div class="menu">
                  <button id="btnCopyDirectLink" onclick="copyDirectLink(${jsArg('#/knowledge/' + encodeURIComponent(e.key))})">Copy link</button>
                  <button onclick="copyToClipboard(${jsArg('kyb get ' + e.key)}, 'CLI command copied!')">Copy CLI command</button>
                  <button onclick="copyEntryBody(${jsArg(e.key)})">Copy as Markdown</button>
                  <button onclick="copyToClipboard(${jsArg(e.key)}, 'Key copied!')">Copy key</button>
                </div>
              </div>
            </div>
          </div>
        </div>

        <div class="detail-body" id="detailBodyContent">
          ${parts.body}
          <div class="prose">${renderMarkdown(e.body)}</div>
          <div id="relatedBox" data-key="${escapeHtml(e.key)}">${relatedHtml(e.key)}</div>
        </div>

        <div class="detail-body" id="detailBodyHistory" style="display:none;">
          <div class="ver" id="verList"></div>
        </div>

        <div class="detail-body" id="detailBodyRaw" style="display:none;">
          <pre style="background:#06080c; border:1px solid var(--border); padding:16px; border-radius:6px; font-family:var(--font-mono); font-size: 13px; color:var(--text); white-space:pre-wrap;">${escapeHtml(e.body)}</pre>
        </div>
      `;

      // The pane is rebuilt on every 4s refresh; the reader's tab must survive it.
      App.currentHistoryList = historyList;
      App.verLoading = isSkeleton;
      if (!isSkeleton) loadRelated(e.key);
      const sameKey = App.verKey === e.key;
      if (!sameKey) { App.detailTab = 'content'; App.verTextOpen = new Set(); }
      App.verKey = e.key;
      renderVersions();
      if (sameKey && App.detailTab && App.detailTab !== 'content') switchDetailTab(App.detailTab);
    }

