    // Expose helpers globally
    window.focusNode = focusNode;
    window.focusNodeById = focusNodeById;
    window.filterFleetByHub = filterFleetByHub;
    window.fitGraphCamera = fitGraphCamera;
    window.zoomGraph = zoomGraph;
    window.resetGraphCamera = resetGraphCamera;
    window.reheatSimulation = reheatSimulation;
    window.setGraphLayout = setGraphLayout;
    window.unpinAllGraphNodes = unpinAllGraphNodes;
    window.openGraphDrawer = openGraphDrawer;
    window.closeGraphDrawer = closeGraphDrawer;

    // Modal Helpers
    function openModal(id) {
      // One dialog at a time: a modal opened from inside another replaces it.
      document.querySelectorAll('.modal-backdrop.open').forEach(m => { if (m.id !== id) m.classList.remove('open'); });
      document.getElementById(id).classList.add('open');
    }
    function closeModal(id) {
      const el = document.getElementById(id);
      if (el) el.classList.remove('open');
      if (document.activeElement && ['INPUT', 'TEXTAREA', 'SELECT', 'BUTTON'].includes(document.activeElement.tagName)) {
        document.activeElement.blur();
      }
    }
    function handleBackdropClick(e, id) {
      if (e.target.id === id) closeModal(id);
    }

    function openNewModal(defaultType = 'knowledge') {
      switchNewModalType(defaultType);
      openModal('newModal');
    }

    // Forms ask only what cannot be derived. The key is derived from the title and shown as
    // one editable mono line; the author is the console's owner and is not asked for.
    const FORM_AUTHOR = 'Alexander Panasenko <alex@prod.codes>';
    const SEVERITIES = [['low', 'Low'], ['medium', 'Medium'], ['high', 'High'], ['critical', 'Critical']];
    const PRIORITIES = [['', 'Unranked'], ['low', 'Low'], ['medium', 'Medium'], ['high', 'High'], ['critical', 'Critical']];

    function segmented(id, options, value) {
      return `<input type="hidden" id="${id}" value="${escapeHtml(value)}"><div class="seg" data-for="${id}">${options.map(([v, l]) =>
        `<button type="button" class="${v === value ? 'on' : ''}" data-value="${escapeHtml(v)}" onclick="segPick(this)">${l}</button>`).join('')}</div>`;
    }
    function segPick(btn) {
      const seg = btn.parentElement;
      document.getElementById(seg.dataset.for).value = btn.dataset.value;
      [...seg.children].forEach(b => b.classList.toggle('on', b === btn));
    }
    const field = (label, control, hint) => `<div class="form-group"><label class="form-label">${label}</label>${control}${hint ? `<div class="form-hint">${hint}</div>` : ''}</div>`;
    const keyField = (id, value, placeholder) => `<div class="form-group key"><label class="form-label">Key</label><input type="text" class="form-input mono" id="${id}" value="${escapeHtml(value || '')}" placeholder="${placeholder}" oninput="this.dataset.touched = 'true'"></div>`;

    function switchNewModalType(type) {
      App.newModalKind = type;
      ['Knowledge', 'Incident', 'Task'].forEach(t => {
        const btn = document.getElementById(`modalType${t}`);
        if (btn) btn.classList.toggle('active', t.toLowerCase() === type);
      });

      const bodyEl = document.getElementById('newModalBody');
      const titleEl = document.getElementById('newModalTitle');
      const submitEl = document.getElementById('newModalSubmitBtn');

      if (type === 'knowledge') {
        titleEl.textContent = 'New note';
        submitEl.textContent = 'Save note';
        bodyEl.innerHTML = `
          ${field('What is it about', `<input type="text" class="form-input" id="newTitle" placeholder="Postgres replicas and failover" oninput="autoSlugKey(this.value)">`)}
          ${keyField('newKey', '', 'postgres-replicas')}
          ${field('Tags', `<input type="text" class="form-input" id="newTags" placeholder="database, postgres, infra">`, 'Comma-separated. Existing tags are suggested in the list.')}
          ${field('The note', `<textarea class="form-textarea" id="newBody" placeholder="Hosts, ports, wiring, decisions and why. Markdown."></textarea>`)}
        `;
      } else if (type === 'incident') {
        titleEl.textContent = 'New incident';
        submitEl.textContent = 'Open incident';
        const todaySlug = new Date().toISOString().slice(0, 10).replace(/-/g, '');
        bodyEl.innerHTML = `
          ${field('What broke', `<input type="text" class="form-input" id="newTitle" placeholder="Redis out of memory on cache-01" oninput="autoSlugIncident(this.value)">`)}
          ${keyField('newKey', `inc-${todaySlug}-`, '')}
          <div class="form-row">
            ${field('Service', `<input type="text" class="form-input" id="newService" placeholder="redis">`)}
            ${field('Which hosts', `<input type="text" class="form-input" id="newHosts" placeholder="cache-01, worker-02">`)}
          </div>
          ${field('How bad', segmented('newSeverity', SEVERITIES, 'medium'))}
          ${field('Is it still happening?', `<input type="text" class="form-input mono" id="newDetection" placeholder="curl -fsS http://cache-01:6379/ping">`, 'A command anyone can run to tell. Without it the incident lands in the unverifiable queue.')}
          ${field('What happened', `<textarea class="form-textarea" id="newBody" placeholder="Symptoms, impact, what was done so far."></textarea>`)}
        `;
      } else if (type === 'task') {
        titleEl.textContent = 'New task';
        submitEl.textContent = 'Add task';
        bodyEl.innerHTML = `
          ${field('What needs doing', `<input type="text" class="form-input" id="newTitle" placeholder="Migrate sccache to local ssd" oninput="autoSlugTask(this.value)">`)}
          ${keyField('newKey', '', 'task-migrate-sccache')}
          <div class="form-row">
            ${field('Priority', segmented('newPriority', PRIORITIES, ''))}
            ${field('Who', `<input type="text" class="form-input" id="newAssignee" value="Alexander Panasenko">`)}
          </div>
          ${field('Done when', `<textarea class="form-textarea" id="newBody" placeholder="What to do and how to tell it is done."></textarea>`)}
        `;
      }
    }

    // A missing field is marked where it is, not only in a toast.
    function markRequired(ids) {
      let first = null;
      ids.forEach(id => {
        const el = document.getElementById(id); if (!el) return;
        const missing = !el.value.trim();
        el.classList.toggle('invalid', missing);
        const group = el.closest('.form-group'); if (!group) return;
        let err = group.querySelector('.form-err');
        if (missing && !err) { err = document.createElement('div'); err.className = 'form-err'; err.textContent = 'This is needed.'; group.appendChild(err); }
        if (!missing && err) err.remove();
        if (missing && !first) first = el;
      });
      if (first) first.focus();
      return !first;
    }

    function autoSlugKey(val) {
      const keyEl = document.getElementById('newKey');
      if (keyEl && (!keyEl.dataset.touched || keyEl.dataset.touched === 'false')) {
        keyEl.value = val.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
      }
    }
    function autoSlugIncident(val) {
      const keyEl = document.getElementById('newKey');
      if (keyEl.dataset.touched === 'true') return;
      const todaySlug = new Date().toISOString().slice(0, 10).replace(/-/g, '');
      const slug = val.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
      keyEl.value = `inc-${todaySlug}-${slug}`;
    }
    function autoSlugTask(val) {
      const keyEl = document.getElementById('newKey');
      if (keyEl.dataset.touched === 'true') return;
      const slug = val.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
      keyEl.value = `task-${slug}`;
    }

    async function submitNewModal() {
      const title = document.getElementById('newTitle')?.value.trim();
      const key = document.getElementById('newKey')?.value.trim();
      const body = document.getElementById('newBody')?.value.trim();

      if (!markRequired(['newTitle', 'newKey', 'newBody'])) {
        showToast('Fill in the marked fields', 'error');
        return;
      }

      let res = null;
      if (App.newModalKind === 'knowledge') {
        const rawTags = document.getElementById('newTags')?.value || '';
        const tags = rawTags.split(',').map(s => s.trim()).filter(Boolean);
        res = await apiPost('/knowledge', {
          key,
          title,
          tags,
          body,
          author: FORM_AUTHOR
        });
      } else if (App.newModalKind === 'incident') {
        const service = document.getElementById('newService')?.value.trim() || 'infrastructure';
        const severity = document.getElementById('newSeverity')?.value || 'medium';
        const rawHosts = document.getElementById('newHosts')?.value || '';
        const hosts = rawHosts.split(',').map(s => s.trim()).filter(Boolean);
        const detection = document.getElementById('newDetection')?.value.trim() || '';
        res = await apiPost('/incidents', {
          key,
          title,
          service,
          severity,
          hosts,
          body,
          ...(detection ? { detection } : {}),
          author: FORM_AUTHOR
        });
      } else if (App.newModalKind === 'task') {
        const priority = document.getElementById('newPriority')?.value || '';
        const assignee = document.getElementById('newAssignee')?.value.trim() || 'Alexander Panasenko';
        res = await apiPost('/tasks', {
          key,
          title,
          priority,
          assignee,
          body,
          author: FORM_AUTHOR
        });
      }

      if (res && res.ok) {
        closeModal('newModal');
        showToast(`Created ${App.newModalKind}: ${key}`, 'success');
        await refreshAll();
        selectEntry(key);
        if (App.newModalKind === 'incident') switchTab('incidents');
        else if (App.newModalKind === 'task') switchTab('tasks');
        else switchTab('search');
      } else {
        showToast(`Failed to save: ${res?.data?.error || res?.error || 'Server error'}`, 'error');
      }
    }

    let editingEntryKey = null;
    let editingEntryKind = 'knowledge';
    // The entry as loaded: an upsert replaces the whole record, so every field
    // the form does not show (status, blocked reason, refs, detection, ...) must
    // go back exactly as it came, or saving a title edit silently reopens a task.
    let editingEntry = null;

    async function openEditModal(key) {
      if (!key) {
        key = App.selectedKey;
      }
      if (!key) {
        showToast('No entry selected to edit', 'error');
        return;
      }
      editingEntryKey = key;
      const entry = await apiFetch(`/knowledge/${encodeURIComponent(key)}`);
      if (!entry) {
        showToast(`Failed to load ${key} for editing`, 'error');
        return;
      }

      editingEntry = entry;
      editingEntryKind = entry.kind || 'knowledge';
      document.getElementById('editModalTitle').textContent = `Edit ${entry.key}`;

      const bodyEl = document.getElementById('editModalBody');
      const hidden = `<input type="hidden" id="editKey" value="${escapeHtml(entry.key)}">`;

      if (editingEntryKind === 'incident') {
        bodyEl.innerHTML = `${hidden}
          ${field('What broke', `<input type="text" class="form-input" id="editTitle" value="${escapeHtml(entry.title || '')}">`)}
          <div class="form-row">
            ${field('Service', `<input type="text" class="form-input" id="editService" value="${escapeHtml(entry.service || '')}">`)}
            ${field('Which hosts', `<input type="text" class="form-input" id="editHosts" value="${escapeHtml((entry.hosts || []).join(', '))}">`)}
          </div>
          ${field('How bad', segmented('editSeverity', SEVERITIES, entry.severity || 'medium'))}
          ${field('Is it still happening?', `<input type="text" class="form-input mono" id="editDetection" value="${escapeHtml(entry.detection || '')}" placeholder="a command anyone can run to tell">`)}
          ${field('What happened', `<textarea class="form-textarea" id="editBody" style="min-height:140px;">${escapeHtml(entry.body || '')}</textarea>`)}
        `;
      } else if (editingEntryKind === 'task') {
        bodyEl.innerHTML = `${hidden}
          ${field('What needs doing', `<input type="text" class="form-input" id="editTitle" value="${escapeHtml(entry.title || '')}">`)}
          <div class="form-row">
            ${field('Priority', segmented('editPriority', PRIORITIES, entry.priority || ''))}
            ${field('Who', `<input type="text" class="form-input" id="editAssignee" value="${escapeHtml(entry.assignee || 'Alexander Panasenko')}">`)}
          </div>
          ${field('Done when', `<textarea class="form-textarea" id="editBody" style="min-height:140px;">${escapeHtml(entry.body || '')}</textarea>`)}
        `;
      } else {
        bodyEl.innerHTML = `${hidden}
          ${field('What is it about', `<input type="text" class="form-input" id="editTitle" value="${escapeHtml(entry.title || '')}">`)}
          ${field('Tags', `<input type="text" class="form-input" id="editTags" value="${escapeHtml((entry.tags || []).join(', '))}">`)}
          ${field('The note', `<textarea class="form-textarea" id="editBody" style="min-height:160px;">${escapeHtml(entry.body || '')}</textarea>`)}
        `;
      }

      openModal('editModal');
    }

    function editKeep(fields) {
      const out = {};
      for (const f of fields) if (editingEntry && editingEntry[f] != null) out[f] = editingEntry[f];
      return out;
    }

    async function submitEditModal() {
      const key = document.getElementById('editKey')?.value;
      const title = document.getElementById('editTitle')?.value.trim();
      const body = document.getElementById('editBody')?.value.trim();
      const author = FORM_AUTHOR;

      if (!markRequired(['editTitle', 'editBody'])) {
        showToast('Fill in the marked fields', 'error');
        return;
      }

      let res = null;
      if (editingEntryKind === 'incident') {
        const service = document.getElementById('editService')?.value.trim() || 'infrastructure';
        const severity = document.getElementById('editSeverity')?.value || 'medium';
        const rawHosts = document.getElementById('editHosts')?.value || '';
        const hosts = rawHosts.split(',').map(s => s.trim()).filter(Boolean);
        const detection = document.getElementById('editDetection')?.value.trim() || '';
        res = await apiPost('/incidents', {
          ...editKeep(['status', 'knowledge', 'resolution', 'affected', 'started_at', 'detected_at', 'tags', 'refs']),
          key,
          title,
          service,
          severity,
          hosts,
          body,
          ...(detection ? { detection } : {}),
          author
        });
      } else if (editingEntryKind === 'task') {
        const priority = document.getElementById('editPriority')?.value || '';
        const assignee = document.getElementById('editAssignee')?.value.trim() || 'Alexander Panasenko';
        res = await apiPost('/tasks', {
          ...editKeep(['status', 'blocked_reason', 'parent_task', 'knowledge', 'resolution', 'tags', 'refs']),
          key,
          title,
          priority,
          assignee,
          body,
          author
        });
      } else {
        const rawTags = document.getElementById('editTags')?.value || '';
        const tags = rawTags.split(',').map(s => s.trim()).filter(Boolean);
        res = await apiPost('/knowledge', {
          ...editKeep(['refs']),
          key,
          title,
          tags,
          body,
          author
        });
      }

      if (res && res.ok) {
        closeModal('editModal');
        showToast(`Saved changes to ${key}`, 'success');
        await refreshAll();
        selectEntry(key);
      } else {
        showToast(`Failed to update: ${res?.data?.error || res?.error || 'Server error'}`, 'error');
      }
    }

    function openShortcutsModal() {
      openModal('shortcutsModal');
    }

