    // Tasks Kanban Rendering & Actions
    // Tasks in the same language as incidents: three counts a person acts on,
    // the filter shown as the command it is, rows underneath. Nothing hides
    // behind the cursor - a blocked task shows its reason in the row.
    const TASK_LIVE = t => !['done','closed','dropped','resolved'].includes((t.status || 'open').toLowerCase()) && !t.archived;
    const TASK_QUEUES = [
      { id: 'blocked', label: 'blocked', color: 'var(--amber)',
        why: 'waiting on someone or something named in the reason',
        flag: '--status blocked',
        test: t => TASK_LIVE(t) && (t.status || '').toLowerCase() === 'blocked' },
      { id: 'stalled', label: 'stalled', color: 'var(--accent)',
        why: 'in progress, nothing changed for more than 3 days',
        flag: '--status in_progress --stale 3d',
        test: t => TASK_LIVE(t) && (t.status || '').toLowerCase() === 'in_progress' && (Date.now() - new Date(t.updated_at || 0).getTime()) > 3 * 86400e3 },
      { id: 'unowned', label: 'unowned', color: 'var(--text)',
        why: 'open, no assignee - nobody will pick it up by accident',
        flag: '--status open --unassigned',
        test: t => TASK_LIVE(t) && (t.status || 'open').toLowerCase() === 'open' && !(t.assignee || '').trim() },
    ];
    function taskSelectQueue(id) { App.taskQueue = App.taskQueue === id ? null : id; App.taskCursor = 0; renderTasks(); }
    function taskSetStatus(st) { App.taskStatusFilter = st; App.taskCursor = 0; renderTasks(); }
    function taskFiltered() {
      let list = [...App.tasks];
      const f = App.taskStatusFilter || 'live';
      if (f === 'live') list = list.filter(TASK_LIVE);
      else if (f === 'done') list = list.filter(t => !TASK_LIVE(t));
      else list = list.filter(t => TASK_LIVE(t) && (t.status || 'open').toLowerCase() === f);
      if (App.taskQueue) { const q = TASK_QUEUES.find(q => q.id === App.taskQueue); if (q) list = list.filter(q.test); }
      const prio = { critical: 0, high: 1, medium: 2, normal: 2, low: 3 };
      const sr = { blocked: 0, in_progress: 1, open: 2 };
      list.sort((a, b) => (sr[(a.status||'open').toLowerCase()] ?? 2) - (sr[(b.status||'open').toLowerCase()] ?? 2)
        || (prio[a.priority] ?? 2) - (prio[b.priority] ?? 2)
        || new Date(b.updated_at || 0) - new Date(a.updated_at || 0));
      return list;
    }

    function renderTasks(keepSelection) {
      const top = document.getElementById('taskTop');
      if (!top) return;
      const live = App.tasks.filter(TASK_LIVE);
      const by = st => live.filter(t => (t.status || 'open').toLowerCase() === st).length;
      const f = App.taskStatusFilter || 'live';
      top.innerHTML = `
        <b>${live.length}</b><span>live</span>
        <span style="color:var(--text-muted)">&middot;</span>
        <span>${by('open')} open &middot; <span style="color:var(--accent)">${by('in_progress')}</span> in progress &middot; <span style="color:var(--amber)">${by('blocked')}</span> blocked</span>
        <span style="color:var(--text-muted)">&middot; ${App.tasks.length - live.length} closed</span>
        <span class="inc-chips">
          ${[['live','live'],['open','open'],['in_progress','in progress'],['blocked','blocked'],['done','closed']].map(([k,l]) => `<button class="chip ${f === k ? 'active' : ''}" onclick="taskSetStatus('${k}')">${l}</button>`).join('')}
        </span>`;

      document.getElementById('taskQueue').innerHTML = TASK_QUEUES.map(q => {
        const n = App.tasks.filter(q.test).length;
        return `<div class="b ${App.taskQueue === q.id ? 'on' : ''} ${n ? '' : 'zero'}" onclick="taskSelectQueue('${q.id}')">
          <div class="h"><b style="color:${n ? q.color : ''}">${n}</b><span>${q.label}</span></div>
          <div class="w">${q.why}</div><div class="k">${escapeHtml(q.flag)}</div></div>`;
      }).join('');

      const list = taskFiltered();
      const parts = ['tasks'];
      if (f !== 'live') parts.push('<i>--status</i> ' + f);
      if (App.taskQueue) parts.push(TASK_QUEUES.find(q => q.id === App.taskQueue).flag.replace(/--(\S+)/g, '<i>--$1</i>'));
      document.getElementById('taskCmd').innerHTML =
        `<span class="p">kyb</span><span>${parts.join(' ')}</span><span class="cur"></span>
         <span class="cnt">${list.length} of ${App.tasks.length}${f === 'live' ? ' &middot; live only' : ''}</span>`;

      if (keepSelection && App.taskCursorKey) { const at = list.findIndex(t => t.key === App.taskCursorKey); if (at >= 0) App.taskCursor = at; }
      if (App.taskCursor == null || App.taskCursor >= list.length) App.taskCursor = 0;
      App.taskCursorKey = list.length ? list[App.taskCursor].key : null;
      const rows = document.getElementById('taskRows');
      if (!list.length) { rows.innerHTML = `<div class="inc-empty">Nothing matches. <kbd>esc</kbd> clears the queue filter.</div>`; return; }
      rows.innerHTML = list.map((t, idx) => {
        const cur = idx === App.taskCursor;
        const st = (t.status || 'open').toLowerCase();
        const done = !TASK_LIVE(t);
        const pr = (t.priority || '').toLowerCase();
        const dot = done ? 'resolved' : pr === 'critical' ? 'critical' : pr === 'high' ? 'high' : '';
        const age = Math.max(0, Math.floor((Date.now() - new Date(t.updated_at || 0).getTime()) / 86400e3));
        const note = st === 'blocked' && t.blocked_reason
          ? `<div class="inc-note"><b>Blocked</b> &middot; ${escapeHtml(t.blocked_reason)}${t.parent_task ? ` <span class="parent">&middot; part of <a href="#/knowledge/${encodeURIComponent(t.parent_task)}" onclick="handleCardClick(event, ${jsArg(t.parent_task)}, 'knowledge')">${escapeHtml(t.parent_task)}</a></span>` : ''}</div>`
          : done && t.resolution ? `<div class="inc-note res"><b>${st === 'dropped' ? 'Dropped' : 'Done'}</b> &middot; ${escapeHtml(t.resolution)}</div>`
          : t.parent_task ? `<div class="inc-note" style="border-left-color:var(--border)"><span class="parent">part of <a href="#/knowledge/${encodeURIComponent(t.parent_task)}" onclick="handleCardClick(event, ${jsArg(t.parent_task)}, 'knowledge')">${escapeHtml(t.parent_task)}</a></span></div>` : '';
        const acts = cur ? `<div class="acts" onclick="event.stopPropagation()">
            <a href="#/knowledge/${encodeURIComponent(t.key)}" class="btn sm primary" onclick="handleCardClick(event, ${jsArg(t.key)}, 'knowledge')">&#9166; Doc</a>
            ${!done ? `<select class="form-select" onchange="moveTaskTo(this.value, ${jsArg(t.key)}, ${jsArg(t.title)}); this.value='';" style="padding:2px 6px; font-size:11px; height:24px; background:var(--card-subtle); color:var(--text-dim); border:1px solid var(--border);">
              <option value="" disabled selected>m Move to…</option>
              <option value="open" ${st === 'open' ? 'disabled' : ''}>Open</option>
              <option value="in_progress" ${st === 'in_progress' ? 'disabled' : ''}>In progress</option>
              <option value="blocked" ${st === 'blocked' ? 'disabled' : ''}>Blocked</option></select>
            <button class="btn sm" onclick="openResolveTaskModal(${jsArg(t.key)}, ${jsArg(t.title)})">x Done</button>` : `<button class="btn sm" onclick="moveTaskTo('open', ${jsArg(t.key)}, ${jsArg(t.title)})">Reopen</button>`}
            <button class="btn sm" onclick="openEditModal(${jsArg(t.key)})">e Edit</button>
            <button class="btn sm" onclick="openDiffForEntry(${jsArg(t.key)})">d Diff</button>
            <button class="btn sm" onclick="openHistoryForEntry(${jsArg(t.key)})">h History</button>
          </div>` : '';
        return `<div class="inc-r task ${cur ? 'cur' : ''}" onclick="App.taskCursor=${idx}; renderTasks();" ondblclick="handleCardClick(event, ${jsArg(t.key)}, 'knowledge')">
            <span class="n">${idx + 1}</span>
            <span class="dot ${dot}"></span>
            <div class="t"><b>${escapeHtml(t.title)}</b><span>${escapeHtml(t.key)}${pr && pr !== 'normal' && pr !== 'medium' ? ' · ' + escapeHtml(pr) : ''}</span></div>
            <span class="st ${st}">${st.replace('_', ' ')}</span>
            <span class="who ${t.assignee ? '' : 'none'}">${escapeHtml(t.assignee || 'unassigned')}</span>
            <span class="age">${age}d</span>
            ${note}${acts}
          </div>`;
      }).join('');
      const curEl = rows.querySelector('.inc-r.cur');
      if (curEl && App.taskKeyNav) { curEl.scrollIntoViewIfNeeded && curEl.scrollIntoViewIfNeeded(false); App.taskKeyNav = false; }
    }

    window.addEventListener('keydown', (e) => {
      if (App.activeTab !== 'tasks') return;
      if (['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName)) return;
      if (document.querySelector('.modal-backdrop.open')) return;
      const list = taskFiltered();
      const cur = list[App.taskCursor || 0];
      const claim = () => { e.preventDefault(); e.stopImmediatePropagation(); };
      if (e.key === 'j' || e.key === 'ArrowDown') { claim(); App.taskCursor = Math.min((App.taskCursor || 0) + 1, list.length - 1); App.taskKeyNav = true; renderTasks(); }
      else if (e.key === 'k' || e.key === 'ArrowUp') { claim(); App.taskCursor = Math.max((App.taskCursor || 0) - 1, 0); App.taskKeyNav = true; renderTasks(); }
      else if (e.key === 'Escape' && App.taskQueue) { claim(); taskSelectQueue(App.taskQueue); }
      else if (!cur) return;
      else if (e.key === 'Enter') { claim(); location.hash = '#/knowledge/' + encodeURIComponent(cur.key); }
      else if (e.key === 'x' && TASK_LIVE(cur)) { claim(); openResolveTaskModal(cur.key, cur.title); }
      else if (e.key === 'm' && TASK_LIVE(cur)) { claim(); const sel = document.querySelector('.inc-r.cur select'); sel && sel.focus(); }
      else if (e.key === 'e') { claim(); openEditModal(cur.key); }
      else if (e.key === 'd') { claim(); openDiffForEntry(cur.key); }
      else if (e.key === 'h') { claim(); openHistoryForEntry(cur.key); }
    }, true);

    function handleDragStart(e, key) {
      draggedTaskKey = key;
      e.dataTransfer.setData('text/plain', key);
      e.target.classList.add('dragging');
    }
    function handleDragOver(e) {
      e.preventDefault();
      e.currentTarget.classList.add('drag-over');
    }
    function handleDragLeave(e) {
      e.currentTarget.classList.remove('drag-over');
    }
    function handleDrop(e, targetStatus) {
      e.preventDefault();
      e.currentTarget.classList.remove('drag-over');
      document.querySelectorAll('.task-card.dragging').forEach(c => c.classList.remove('dragging'));
      if (!draggedTaskKey) return;

      const task = App.tasks.find(t => t.key === draggedTaskKey);
      moveTaskTo(targetStatus, draggedTaskKey, task?.title || draggedTaskKey);
      draggedTaskKey = null;
    }

    function moveTaskTo(targetStatus, key, title) {
      if (!targetStatus) return;
      if (targetStatus === 'done') {
        openResolveTaskModal(key, title);
      } else if (targetStatus === 'blocked') {
        promptBlockTask(key, title);
      } else {
        transitionTask(key, targetStatus);
      }
    }

    async function transitionTask(key, status, reason = null, author = 'Alexander Panasenko <alex@prod.codes>') {
      const payload = {
        status,
        author
      };
      if (status === 'blocked' && reason) {
        payload.blocked_reason = reason;
      }
      const res = await apiPost(`/tasks/${encodeURIComponent(key)}/transition`, payload);
      if (res.ok) {
        showToast(`Task ${key} transitioned to ${status}`, 'success');
        App.detailCache.delete(key);
        await refreshAll();
        if (App.selectedKey === key) {
          selectEntry(key, false);
        }
      } else {
        showToast(`Transition failed: ${res.data?.error || res.error || 'Server error'}`, 'error');
      }
    }

    function promptBlockTask(key, title = '') {
      App.activeBlockKey = key;
      const t = App.tasks.find(x => x.key === key);
      const displayTitle = title || t?.title || key;
      const targetEl = document.getElementById('blockTaskTargetText');
      if (targetEl) targetEl.textContent = `Task: ${displayTitle} (${key})`;
      const reasonEl = document.getElementById('blockTaskReason');
      if (reasonEl) reasonEl.value = '';
      openModal('blockTaskModal');
    }

    async function submitBlockTask() {
      const reasonEl = document.getElementById('blockTaskReason');
      const authorEl = document.getElementById('blockTaskAuthor');
      const reason = reasonEl ? reasonEl.value.trim() : '';
      const author = (authorEl ? authorEl.value.trim() : '') || 'Alexander Panasenko <alex@prod.codes>';
      if (!reason) {
        showToast('Please provide a reason why this task is blocked', 'error');
        return;
      }
      closeModal('blockTaskModal');
      await transitionTask(App.activeBlockKey, 'blocked', reason, author);
    }

    function openResolveTaskModal(key, title) {
      App.activeResolveKey = key;
      document.getElementById('resolveTaskTargetText').textContent = `Task: ${title} (${key})`;
      document.getElementById('resolveTaskOutcome').value = '';
      openModal('resolveTaskModal');
    }

    async function submitResolveTask() {
      const outcome = document.getElementById('resolveTaskOutcome').value.trim();
      const author = document.getElementById('resolveTaskAuthor').value.trim() || 'Alexander Panasenko <alex@prod.codes>';
      if (!outcome) {
        showToast('Please provide a completion outcome note', 'error');
        return;
      }

      const res = await apiPost(`/tasks/${encodeURIComponent(App.activeResolveKey)}/resolve`, {
        resolution: outcome
      });

      if (res.ok) {
        closeModal('resolveTaskModal');
        showToast(`Task ${App.activeResolveKey} marked done!`, 'success');
        App.detailCache.delete(App.activeResolveKey);
        await refreshAll();
        if (App.selectedKey === App.activeResolveKey) {
          selectEntry(App.activeResolveKey, false);
        }
      } else {
        showToast(`Failed: ${res.data?.error || res.error || 'Server error'}`, 'error');
      }
    }

