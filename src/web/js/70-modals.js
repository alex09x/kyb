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

    function switchNewModalType(type) {
      App.newModalKind = type;
      ['Knowledge', 'Incident', 'Task'].forEach(t => {
        const btn = document.getElementById(`modalType${t}`);
        if (btn) btn.classList.toggle('active', t.toLowerCase() === type);
      });

      const bodyEl = document.getElementById('newModalBody');
      const titleEl = document.getElementById('newModalTitle');

      if (type === 'knowledge') {
        titleEl.textContent = 'Create Fleet Knowledge Article';
        bodyEl.innerHTML = `
          <div class="form-group">
            <label class="form-label">Title *</label>
            <input type="text" class="form-input" id="newTitle" placeholder="e.g. Postgres replicas and failover" oninput="autoSlugKey(this.value)">
          </div>
          <div class="form-group">
            <label class="form-label">Unique Key *</label>
            <input type="text" class="form-input" id="newKey" placeholder="e.g. postgres-replicas">
          </div>
          <div class="form-group">
            <label class="form-label">Tags (comma-separated)</label>
            <input type="text" class="form-input" id="newTags" placeholder="database, postgres, infra">
          </div>
          <div class="form-group">
            <label class="form-label">Markdown Body *</label>
            <textarea class="form-textarea" id="newBody" placeholder="Document architecture, hosts, ports, wiring, and operational notes..."></textarea>
          </div>
        `;
      } else if (type === 'incident') {
        titleEl.textContent = 'Report Fleet Incident';
        const todaySlug = new Date().toISOString().slice(0, 10).replace(/-/g, '');
        bodyEl.innerHTML = `
          <div class="form-group">
            <label class="form-label">Incident Title *</label>
            <input type="text" class="form-input" id="newTitle" placeholder="e.g. Redis out of memory on cache-01" oninput="autoSlugIncident(this.value)">
          </div>
          <div class="form-group">
            <label class="form-label">Incident Key *</label>
            <input type="text" class="form-input" id="newKey" value="inc-${todaySlug}-">
          </div>
          <div style="display:grid; grid-template-columns:1fr 1fr; gap:10px;">
            <div class="form-group">
              <label class="form-label">Service</label>
              <input type="text" class="form-input" id="newService" placeholder="e.g. redis">
            </div>
            <div class="form-group">
              <label class="form-label">Severity</label>
              <select class="form-select" id="newSeverity">
                <option value="medium">Medium</option>
                <option value="high">High</option>
                <option value="critical">Critical</option>
                <option value="low">Low</option>
              </select>
            </div>
          </div>
          <div class="form-group">
            <label class="form-label">Impacted Hosts (comma-separated)</label>
            <input type="text" class="form-input" id="newHosts" placeholder="cache-01, worker-02">
          </div>
          <div class="form-group">
            <label class="form-label">Incident Summary & Detection Check *</label>
            <textarea class="form-textarea" id="newBody" placeholder="What happened, symptoms, impact, and immediate workaround..."></textarea>
          </div>
        `;
      } else if (type === 'task') {
        titleEl.textContent = 'Add Fleet Task / Idea';
        bodyEl.innerHTML = `
          <div class="form-group">
            <label class="form-label">Task Title *</label>
            <input type="text" class="form-input" id="newTitle" placeholder="e.g. Migrate sccache to local ssd" oninput="autoSlugTask(this.value)">
          </div>
          <div class="form-group">
            <label class="form-label">Task Key *</label>
            <input type="text" class="form-input" id="newKey" placeholder="task-migrate-sccache">
          </div>
          <div style="display:grid; grid-template-columns:1fr 1fr; gap:10px;">
            <div class="form-group">
              <label class="form-label">Priority</label>
              <select class="form-select" id="newPriority">
                <option value="">Not ranked</option>
                <option value="low">Low</option>
                <option value="medium">Medium</option>
                <option value="high">High</option>
                <option value="critical">Critical</option>
              </select>
            </div>
            <div class="form-group">
              <label class="form-label">Assignee</label>
              <input type="text" class="form-input" id="newAssignee" placeholder="Alexander Panasenko" value="Alexander Panasenko">
            </div>
          </div>
          <div class="form-group">
            <label class="form-label">Task Description / Acceptance Criteria *</label>
            <textarea class="form-textarea" id="newBody" placeholder="What needs to be done and how to verify..."></textarea>
          </div>
        `;
      }
    }

    function autoSlugKey(val) {
      const keyEl = document.getElementById('newKey');
      if (keyEl && (!keyEl.dataset.touched || keyEl.dataset.touched === 'false')) {
        keyEl.value = val.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
      }
    }
    function autoSlugIncident(val) {
      const keyEl = document.getElementById('newKey');
      const todaySlug = new Date().toISOString().slice(0, 10).replace(/-/g, '');
      const slug = val.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
      keyEl.value = `inc-${todaySlug}-${slug}`;
    }
    function autoSlugTask(val) {
      const keyEl = document.getElementById('newKey');
      const slug = val.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
      keyEl.value = `task-${slug}`;
    }

    async function submitNewModal() {
      const title = document.getElementById('newTitle')?.value.trim();
      const key = document.getElementById('newKey')?.value.trim();
      const body = document.getElementById('newBody')?.value.trim();

      if (!title || !key || !body) {
        showToast('Please fill out all required fields (*)', 'error');
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
          author: 'Alexander Panasenko <alex@prod.codes>'
        });
      } else if (App.newModalKind === 'incident') {
        const service = document.getElementById('newService')?.value.trim() || 'infrastructure';
        const severity = document.getElementById('newSeverity')?.value || 'medium';
        const rawHosts = document.getElementById('newHosts')?.value || '';
        const hosts = rawHosts.split(',').map(s => s.trim()).filter(Boolean);
        res = await apiPost('/incidents', {
          key,
          title,
          service,
          severity,
          hosts,
          body,
          author: 'Alexander Panasenko <alex@prod.codes>'
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
          author: 'Alexander Panasenko <alex@prod.codes>'
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
      document.getElementById('editModalTitle').textContent = `Edit ${editingEntryKind.toUpperCase()}: ${entry.key}`;

      const bodyEl = document.getElementById('editModalBody');

      if (editingEntryKind === 'incident') {
        bodyEl.innerHTML = `
          <input type="hidden" id="editKey" value="${escapeHtml(entry.key)}">
          <div class="form-group">
            <label class="form-label">Incident Title *</label>
            <input type="text" class="form-input" id="editTitle" value="${escapeHtml(entry.title || '')}">
          </div>
          <div style="display:grid; grid-template-columns:1fr 1fr; gap:10px;">
            <div class="form-group">
              <label class="form-label">Service</label>
              <input type="text" class="form-input" id="editService" value="${escapeHtml(entry.service || '')}">
            </div>
            <div class="form-group">
              <label class="form-label">Severity</label>
              <select class="form-select" id="editSeverity">
                <option value="critical" ${entry.severity === 'critical' ? 'selected' : ''}>Critical</option>
                <option value="high" ${entry.severity === 'high' ? 'selected' : ''}>High</option>
                <option value="medium" ${entry.severity === 'medium' || !entry.severity ? 'selected' : ''}>Medium</option>
                <option value="low" ${entry.severity === 'low' ? 'selected' : ''}>Low</option>
              </select>
            </div>
          </div>
          <div class="form-group">
            <label class="form-label">Impacted Hosts (comma-separated)</label>
            <input type="text" class="form-input" id="editHosts" value="${escapeHtml((entry.hosts || []).join(', '))}">
          </div>
          <div class="form-group">
            <label class="form-label">Incident Body / Mitigation Notes *</label>
            <textarea class="form-textarea" id="editBody" style="min-height:140px;">${escapeHtml(entry.body || '')}</textarea>
          </div>
          <div class="form-group">
            <label class="form-label">Author Identity</label>
            <input type="text" class="form-input" id="editAuthor" value="Alexander Panasenko &lt;alex@prod.codes&gt;">
          </div>
        `;
      } else if (editingEntryKind === 'task') {
        bodyEl.innerHTML = `
          <input type="hidden" id="editKey" value="${escapeHtml(entry.key)}">
          <div class="form-group">
            <label class="form-label">Task Title *</label>
            <input type="text" class="form-input" id="editTitle" value="${escapeHtml(entry.title || '')}">
          </div>
          <div style="display:grid; grid-template-columns:1fr 1fr; gap:10px;">
            <div class="form-group">
              <label class="form-label">Priority</label>
              <select class="form-select" id="editPriority">
                ${[['', 'Not ranked'], ['low', 'Low'], ['medium', 'Medium'], ['high', 'High'], ['critical', 'Critical']].map(([v, l]) =>
                  `<option value="${v}" ${(entry.priority || '') === v ? 'selected' : ''}>${l}</option>`).join('')}
              </select>
            </div>
            <div class="form-group">
              <label class="form-label">Assignee</label>
              <input type="text" class="form-input" id="editAssignee" value="${escapeHtml(entry.assignee || 'Alexander Panasenko')}">
            </div>
          </div>
          <div class="form-group">
            <label class="form-label">Task Description / Acceptance Criteria *</label>
            <textarea class="form-textarea" id="editBody" style="min-height:140px;">${escapeHtml(entry.body || '')}</textarea>
          </div>
          <div class="form-group">
            <label class="form-label">Author Identity</label>
            <input type="text" class="form-input" id="editAuthor" value="Alexander Panasenko &lt;alex@prod.codes&gt;">
          </div>
        `;
      } else {
        bodyEl.innerHTML = `
          <input type="hidden" id="editKey" value="${escapeHtml(entry.key)}">
          <div class="form-group">
            <label class="form-label">Title *</label>
            <input type="text" class="form-input" id="editTitle" value="${escapeHtml(entry.title || '')}">
          </div>
          <div class="form-group">
            <label class="form-label">Tags (comma-separated)</label>
            <input type="text" class="form-input" id="editTags" value="${escapeHtml((entry.tags || []).join(', '))}">
          </div>
          <div class="form-group">
            <label class="form-label">Markdown Body *</label>
            <textarea class="form-textarea" id="editBody" style="min-height:160px;">${escapeHtml(entry.body || '')}</textarea>
          </div>
          <div class="form-group">
            <label class="form-label">Author Identity</label>
            <input type="text" class="form-input" id="editAuthor" value="Alexander Panasenko &lt;alex@prod.codes&gt;">
          </div>
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
      const author = document.getElementById('editAuthor')?.value.trim() || 'Alexander Panasenko <alex@prod.codes>';

      if (!title || !body) {
        showToast('Title and body are required', 'error');
        return;
      }

      let res = null;
      if (editingEntryKind === 'incident') {
        const service = document.getElementById('editService')?.value.trim() || 'infrastructure';
        const severity = document.getElementById('editSeverity')?.value || 'medium';
        const rawHosts = document.getElementById('editHosts')?.value || '';
        const hosts = rawHosts.split(',').map(s => s.trim()).filter(Boolean);
        res = await apiPost('/incidents', {
          ...editKeep(['status', 'knowledge', 'resolution', 'detection', 'affected', 'started_at', 'detected_at', 'tags', 'refs']),
          key,
          title,
          service,
          severity,
          hosts,
          body,
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

