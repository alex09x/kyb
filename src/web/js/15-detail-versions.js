    // ---- Versions: the entry's own ledger. Every version, newest first, each
    // with the whole of what it changed. Nothing to select, nothing to click.
    App.verDiffs = App.verDiffs || new Map();   // `${key}@${sha}` -> diff | {created:true, lines}
    App.verTextOpen = App.verTextOpen || new Set();
    App.verSnaps = App.verSnaps || new Map();
    // The list is passed in, never read from App here: a load that outlives a
    // switch to another entry must still pair each version with its own parent.
    function verPrevSha(h, sha) {
      const i = h.findIndex(v => v.sha === sha);
      return i >= 0 && i < h.length - 1 ? h[i + 1].sha : null;
    }
    function verWho(v) { return (v.author || '').replace(/\s*<[^>]*>/, '').trim(); }
    function verWhen(v) { return new Date(v.committed_at).toLocaleString(undefined, { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit', hour12: false }); }

    function renderVersions() {
      const el = document.getElementById('verList');
      if (!el) return;
      const h = App.currentHistoryList || [], key = App.verKey;
      if (!h.length) { el.innerHTML = '<div class="ver-empty">No versions recorded for this entry.</div>'; return; }
      el.innerHTML = `<div class="ver-h"><span>${h.length} version${h.length === 1 ? '' : 's'} &middot; every change, in full</span><span class="mono">kyb history ${escapeHtml(key)}</span></div>` +
        h.map((v, i) => {
          const n = h.length - i, id = `${key}@${v.sha}`, d = App.verDiffs.get(id);
          const who = verWho(v);
          let stat = '<span class="dim">&hellip;</span>', body = '';
          if (d && d.created) {
            stat = `<ins>+${d.lines}</ins>`;
            body = `<div class="ver-note">created &middot; ${d.lines} lines &middot; <a href="javascript:void(0)" onclick="verToggleText(${jsArg(v.sha)})">${App.verTextOpen.has(v.sha) ? 'hide' : 'show'} the document as first written</a></div>`;
          } else if (d) {
            const fields = (d.fields || []).filter(f => f.field !== 'updated_at');
            const rm = (d.body?.removed || []).filter(l => l.trim()), ad = (d.body?.added || []).filter(l => l.trim());
            stat = `${ad.length ? `<ins>+${ad.length}</ins>` : ''} ${rm.length ? `<em>&minus;${rm.length}</em>` : ''}${!ad.length && !rm.length ? `<span>${fields.length} field${fields.length === 1 ? '' : 's'}</span>` : ''}`;
            if (fields.length) body += `<div class="ver-fld">${fields.map(f => `<span class="k">${escapeHtml(f.field)}</span><span class="chg">${f.from ? `<em>${escapeHtml(String(f.from))}</em> &rarr; ` : ''}<ins>${escapeHtml(String(f.to ?? ''))}</ins></span>`).join('')}</div>`;
            if (rm.length || ad.length) body += `<div class="ver-dif">${rm.map(l => `<div class="d"><span>&minus;</span><span>${escapeHtml(l)}</span></div>`).join('')}${ad.map(l => `<div class="a"><span>+</span><span>${escapeHtml(l)}</span></div>`).join('')}</div>`;
            if (!fields.length && !rm.length && !ad.length) body += '<div class="ver-note">nothing in the text or the fields changed in this version</div>';
          }
          const snap = App.verTextOpen.has(v.sha) ? (App.verSnaps.get(id) ? `<div class="prose ver-text"><h3 style="margin-top:0">${escapeHtml(App.verSnaps.get(id).title || '')}</h3>${renderMarkdown(App.verSnaps.get(id).body || '')}</div>` : '<div class="ver-note">Loading&hellip;</div>') : '';
          return `<div class="ver-ev">
            <div class="ver-bh"><span class="mono vn">v${n}</span><span>${verWhen(v)}</span>${who ? `<span>${escapeHtml(who)}</span>` : ''}<span class="mono dim">${v.sha.slice(0, 7)}</span><span class="pm mono">${stat}</span>
              ${!(d && d.created) ? `<a class="tx" href="javascript:void(0)" onclick="verToggleText(${jsArg(v.sha)})">${App.verTextOpen.has(v.sha) ? 'hide text' : 'text at v' + n}</a>` : ''}</div>
            ${body}${snap}
          </div>`;
        }).join('');
      verLoadDiffs();
    }
    async function verLoadDiffs() {
      const key = App.verKey, all = (App.currentHistoryList || []).slice(), h = all.slice(0, 25);
      const want = h.filter(v => !App.verDiffs.has(`${key}@${v.sha}`));
      const textWant = h.filter(v => App.verTextOpen.has(v.sha) && !App.verSnaps.has(`${key}@${v.sha}`));
      if (!want.length && !textWant.length) return;
      await Promise.all([
        ...want.map(async v => {
          const prev = verPrevSha(all, v.sha);
          let d = null;
          if (prev) d = await apiFetch(`/knowledge/${encodeURIComponent(key)}/diff?from=${prev}&to=${v.sha}`);
          else { const snap = await apiFetch(`/knowledge/${encodeURIComponent(key)}?at=${v.sha}`); if (snap) { App.verSnaps.set(`${key}@${v.sha}`, snap); d = { created: true, lines: (snap.body || '').split('\n').filter(l => l.trim()).length }; } }
          // a failed request is not a version with no changes: leave it unloaded
          if (d) App.verDiffs.set(`${key}@${v.sha}`, d);
        }),
        ...textWant.map(async v => { const snap = await apiFetch(`/knowledge/${encodeURIComponent(key)}?at=${v.sha}`); if (snap) App.verSnaps.set(`${key}@${v.sha}`, snap); }),
      ]);
      if (App.verKey === key) renderVersions();
    }
    function verToggleText(sha) {
      App.verTextOpen.has(sha) ? App.verTextOpen.delete(sha) : App.verTextOpen.add(sha);
      renderVersions();
    }

    function copyEntryBody() {
      if (App.currentDetailBody) {
        copyToClipboard(App.currentDetailBody, 'Markdown content copied!');
      }
    }

    function copyDirectLink(hashPath) {
      const fullUrl = window.location.origin + window.location.pathname + hashPath;
      copyToClipboard(fullUrl, `Direct link copied: ${hashPath}`);
    }

    function fallbackCopy(text, msg) {
      try {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
        showToast(msg, 'success');
      } catch (err) {
        showToast('Clipboard access denied', 'error');
      }
    }

    function copyToClipboard(text, msg) {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(() => {
          showToast(msg, 'success');
        }).catch(() => {
          fallbackCopy(text, msg);
        });
      } else {
        fallbackCopy(text, msg);
      }
    }








    async function openHistoryForEntry(key) {
      if (!key) return;
      await selectEntry(key);
      switchDetailTab('history');
      switchTab('search');
    }

    async function openDiffForEntry(key) {
      if (!key) return;
      await selectEntry(key);
      switchDetailTab('diff');
      switchTab('search');
    }


    async function inspectRevision(key, sha) {
      showToast(`Loading snapshot ${sha.slice(0, 7)}...`, 'info');
      const snap = await apiFetch(`/knowledge/${encodeURIComponent(key)}?at=${encodeURIComponent(sha)}`);
      if (!snap) {
        showToast('Failed to load historical snapshot', 'error');
        return;
      }
      const contentEl = document.getElementById('detailBodyContent');
      if (contentEl) {
        contentEl.innerHTML = `
          <div style="background:var(--accent-dim); border:1px solid var(--accent); border-radius:6px; padding:10px 14px; margin-bottom:14px; display:flex; justify-content:space-between; align-items:center;">
            <div>
              <span style="font-weight:700; color:var(--accent);">Historical Snapshot:</span>
              <span style="font-family:var(--font-mono); font-size: 13px; margin-left:6px;">commit ${sha.slice(0, 7)}</span>
              <span style="font-size: 12.5px; color:var(--text-muted); margin-left:8px;">${escapeHtml(snap.author || '')}</span>
            </div>
            <button class="btn sm" onclick="selectEntry(${jsArg(key)})">Back to Head</button>
          </div>
          <div class="prose">${renderMarkdown(snap.body)}</div>
        `;
        switchDetailTab('content');
      }
    }

    function switchDetailTab(tab) {
      document.querySelectorAll('.detail-tab').forEach(b => b.classList.remove('active'));
      const want = tab === 'history' || tab === 'diff' ? 'versions' : tab === 'content' ? 'document' : tab;
      const activeBtn = Array.from(document.querySelectorAll('.detail-tab')).find(b => b.textContent.toLowerCase().includes(want));
      if (activeBtn) activeBtn.classList.add('active');

      if (tab === 'diff') tab = 'history';
      App.detailTab = tab;
      const show = (id, on) => { const el = document.getElementById(id); if (el) el.style.display = on ? 'block' : 'none'; };
      show('detailBodyContent', tab === 'content');
      show('detailBodyHistory', tab === 'history');
      show('detailBodyRaw', tab === 'raw');
    }

