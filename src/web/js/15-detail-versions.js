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

    let currentDiffData = null;

    function computeLineDiff(textA, textB) {
      const linesA = (textA !== undefined && textA !== null) ? String(textA).split('\n') : [];
      const linesB = (textB !== undefined && textB !== null) ? String(textB).split('\n') : [];
      const n = linesA.length;
      const m = linesB.length;

      if (n === 0 && m === 0) {
        return { splitRows: [], unifiedRows: [], linesA, linesB, hasChanges: false };
      }
      if (n === 0) {
        const splitRows = linesB.map((b, idx) => ({
          left: null,
          right: { num: idx + 1, text: b, type: 'add' }
        }));
        const unifiedRows = linesB.map((b, idx) => ({
          oldNum: null,
          newNum: idx + 1,
          type: 'add',
          mark: '+',
          text: b
        }));
        return { splitRows, unifiedRows, linesA, linesB, hasChanges: true };
      }
      if (m === 0) {
        const splitRows = linesA.map((a, idx) => ({
          left: { num: idx + 1, text: a, type: 'del' },
          right: null
        }));
        const unifiedRows = linesA.map((a, idx) => ({
          oldNum: idx + 1,
          newNum: null,
          type: 'del',
          mark: '-',
          text: a
        }));
        return { splitRows, unifiedRows, linesA, linesB, hasChanges: true };
      }

      if (n > 1500 || m > 1500) {
        const maxLen = Math.max(n, m);
        const splitRows = [];
        const unifiedRows = [];
        for (let i = 0; i < maxLen; i++) {
          const a = i < n ? linesA[i] : null;
          const b = i < m ? linesB[i] : null;
          if (a === b) {
            splitRows.push({
              left: { num: i + 1, text: a, type: 'equal' },
              right: { num: i + 1, text: b, type: 'equal' }
            });
            unifiedRows.push({ oldNum: i + 1, newNum: i + 1, type: 'equal', mark: ' ', text: a });
          } else {
            if (a !== null) {
              splitRows.push({
                left: { num: i + 1, text: a, type: 'del' },
                right: b !== null ? { num: i + 1, text: b, type: 'add' } : null
              });
              unifiedRows.push({ oldNum: i + 1, newNum: null, type: 'del', mark: '-', text: a });
            }
            if (b !== null && a === null) {
              splitRows.push({
                left: null,
                right: { num: i + 1, text: b, type: 'add' }
              });
              unifiedRows.push({ oldNum: null, newNum: i + 1, type: 'add', mark: '+', text: b });
            }
          }
        }
        return { splitRows, unifiedRows, linesA, linesB, hasChanges: true };
      }

      const dp = Array.from({ length: n + 1 }, () => new Int32Array(m + 1));
      for (let i = 0; i < n; i++) {
        for (let j = 0; j < m; j++) {
          if (linesA[i] === linesB[j]) {
            dp[i + 1][j + 1] = dp[i][j] + 1;
          } else {
            dp[i + 1][j + 1] = Math.max(dp[i + 1][j], dp[i][j + 1]);
          }
        }
      }

      let i = n, j = m;
      const ops = [];
      while (i > 0 || j > 0) {
        if (i > 0 && j > 0 && linesA[i - 1] === linesB[j - 1]) {
          ops.push({ type: 'equal', lineA: linesA[i - 1], lineB: linesB[j - 1], numA: i, numB: j });
          i--; j--;
        } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
          ops.push({ type: 'add', lineB: linesB[j - 1], numB: j });
          j--;
        } else {
          ops.push({ type: 'del', lineA: linesA[i - 1], numA: i });
          i--;
        }
      }
      ops.reverse();

      let hasChanges = false;
      const unifiedRows = [];
      for (const op of ops) {
        if (op.type === 'equal') {
          unifiedRows.push({ oldNum: op.numA, newNum: op.numB, type: 'equal', mark: ' ', text: op.lineA });
        } else if (op.type === 'del') {
          hasChanges = true;
          unifiedRows.push({ oldNum: op.numA, newNum: null, type: 'del', mark: '-', text: op.lineA });
        } else if (op.type === 'add') {
          hasChanges = true;
          unifiedRows.push({ oldNum: null, newNum: op.numB, type: 'add', mark: '+', text: op.lineB });
        }
      }

      const splitRows = [];
      let k = 0;
      while (k < ops.length) {
        const op = ops[k];
        if (op.type === 'equal') {
          splitRows.push({
            left: { num: op.numA, text: op.lineA, type: 'equal' },
            right: { num: op.numB, text: op.lineB, type: 'equal' }
          });
          k++;
        } else {
          const dels = [];
          const adds = [];
          while (k < ops.length && ops[k].type !== 'equal') {
            if (ops[k].type === 'del') dels.push(ops[k]);
            else if (ops[k].type === 'add') adds.push(ops[k]);
            k++;
          }
          const maxLen = Math.max(dels.length, adds.length);
          for (let r = 0; r < maxLen; r++) {
            const d = dels[r];
            const a = adds[r];
            splitRows.push({
              left: d ? { num: d.numA, text: d.lineA, type: 'del' } : null,
              right: a ? { num: a.numB, text: a.lineB, type: 'add' } : null
            });
          }
        }
      }

      return { splitRows, unifiedRows, linesA, linesB, hasChanges };
    }

    async function loadDiff(key, fromSha, toSha) {
      App.diffCurrentKey = key;
      App.diffFromSha = fromSha;
      App.diffToSha = toSha;

      const container = document.getElementById('diffViewContainer');
      if (!container) return;

      if (!App.currentHistoryList || App.currentHistoryList.length === 0 || (App.currentHistoryList[0] && App.currentHistoryList[0].key !== key)) {
        const hData = await apiFetch(`/knowledge/${encodeURIComponent(key)}/history`);
        if (hData && hData.versions) {
          App.currentHistoryList = hData.versions;
        }
      }
      const historyList = App.currentHistoryList || [];

      let diffData = null;
      try {
        diffData = await apiFetch(`/knowledge/${encodeURIComponent(key)}/diff?from=${encodeURIComponent(fromSha)}&to=${encodeURIComponent(toSha)}`);
      } catch (_) {}

      if (!diffData) {
        const isFromInitial = fromSha === 'initial' || fromSha === 'empty';
        const fromHist = historyList.find(h => h.sha === fromSha);
        const toHist = historyList.find(h => h.sha === toSha);

        const [fromSnap, toSnap] = await Promise.all([
          isFromInitial ? Promise.resolve(null) : (fromHist && fromHist.change === 'deleted' ? Promise.resolve(null) : apiFetch(`/knowledge/${encodeURIComponent(key)}?at=${encodeURIComponent(fromSha)}`)),
          (toHist && toHist.change === 'deleted') ? Promise.resolve(null) : apiFetch(`/knowledge/${encodeURIComponent(key)}?at=${encodeURIComponent(toSha)}`)
        ]);

        const fromEntry = fromSnap || {
          key,
          title: `(Empty Initial State)`,
          body: '',
          status: isFromInitial ? 'initial' : (fromHist && fromHist.change === 'deleted' ? 'archived' : 'open')
        };
        const toEntry = toSnap || {
          key,
          title: `(Archived: ${key})`,
          body: '',
          status: (toHist && toHist.change === 'deleted') ? 'archived' : 'open'
        };

        const fields = [];
        if (fromEntry.status !== toEntry.status) {
          fields.push({ field: 'status', from: fromEntry.status || 'open', to: toEntry.status || 'archived' });
        }
        if (fromEntry.title !== toEntry.title && fromEntry.title && toEntry.title) {
          fields.push({ field: 'title', from: fromEntry.title, to: toEntry.title });
        }
        if (fromEntry.assignee !== toEntry.assignee) {
          fields.push({ field: 'assignee', from: fromEntry.assignee || 'unassigned', to: toEntry.assignee || 'unassigned' });
        }
        if (fromEntry.priority !== toEntry.priority) {
          fields.push({ field: 'priority', from: fromEntry.priority || 'normal', to: toEntry.priority || 'normal' });
        }

        diffData = {
          key,
          from: {
            rev: fromSha,
            committed_at: fromHist ? fromHist.committed_at : '',
            author: fromHist ? (fromHist.author || 'Alexander Panasenko <alex@prod.codes>') : 'Initial State',
            message: fromHist ? fromHist.message : 'Initial Creation',
            exists: !isFromInitial && (!fromHist || fromHist.change !== 'deleted')
          },
          to: {
            rev: toSha,
            committed_at: toHist ? toHist.committed_at : '',
            author: toHist ? (toHist.author || 'Alexander Panasenko <alex@prod.codes>') : '',
            message: toHist ? toHist.message : 'Updated',
            exists: !toHist || toHist.change !== 'deleted'
          },
          changed: true,
          fields,
          body: { added: [], removed: [] },
          from_entry: fromEntry,
          to_entry: toEntry
        };
      }

      if (!diffData.from_entry || !diffData.to_entry) {
        const [fSnap, tSnap] = await Promise.all([
          fromSha === 'initial' ? Promise.resolve(null) : apiFetch(`/knowledge/${encodeURIComponent(key)}?at=${encodeURIComponent(fromSha)}`),
          apiFetch(`/knowledge/${encodeURIComponent(key)}?at=${encodeURIComponent(toSha)}`)
        ]);
        if (!diffData.from_entry) diffData.from_entry = fSnap || { key, body: '' };
        if (!diffData.to_entry) diffData.to_entry = tSnap || { key, body: '' };
      }

      currentDiffData = diffData;
      renderCurrentDiff();
    }

    function renderCurrentDiff() {
      const container = document.getElementById('diffViewContainer');
      if (!container || !currentDiffData) return;

      const diff = currentDiffData;
      const historyList = App.currentHistoryList || [];
      const viewMode = App.diffViewMode || 'split';

      const fromSha = (diff.from && diff.from.rev) || App.diffFromSha;
      const toSha = (diff.to && diff.to.rev) || App.diffToSha;

      const fromEntry = diff.from_entry || {};
      const toEntry = diff.to_entry || {};

      const lineDiff = computeLineDiff(fromEntry.body || '', toEntry.body || '');

      let html = `
        <div class="diff-toolbar">
          <div style="display:flex; align-items:center; gap:10px; flex-wrap:wrap;">
            <div class="diff-picker-group">
              <span class="diff-picker-label">До (Было):</span>
              <select id="diffFromSelect" class="diff-select" onchange="onDiffRevisionChange()">
                ${fromSha === 'initial' ? `<option value="initial" selected>(Initial Creation / Empty State)</option>` : ''}
                ${historyList.map(h => `
                  <option value="${escapeHtml(h.sha)}" ${h.sha === fromSha ? 'selected' : ''}>
                    ${h.sha.slice(0, 7)} — ${escapeHtml(h.message || 'commit')} (${timeAgo(h.committed_at)})
                  </option>
                `).join('')}
              </select>
            </div>

            <button class="btn sm" onclick="swapDiffRevisions()" title="Поменять ревизии местами" style="padding:4px 8px;">⇄</button>

            <div class="diff-picker-group">
              <span class="diff-picker-label">После (Стало):</span>
              <select id="diffToSelect" class="diff-select" onchange="onDiffRevisionChange()">
                ${historyList.map(h => `
                  <option value="${escapeHtml(h.sha)}" ${h.sha === toSha ? 'selected' : ''}>
                    ${h.sha.slice(0, 7)} — ${escapeHtml(h.message || 'commit')} (${timeAgo(h.committed_at)})
                  </option>
                `).join('')}
              </select>
            </div>
          </div>

          <div style="display:flex; align-items:center; gap:8px;">
            <div class="diff-view-toggles">
              <button class="diff-view-toggle-btn ${viewMode === 'split' ? 'active' : ''}" onclick="setDiffViewMode('split')">Split (До / После)</button>
              <button class="diff-view-toggle-btn ${viewMode === 'unified' ? 'active' : ''}" onclick="setDiffViewMode('unified')">Unified</button>
            </div>
          </div>
        </div>
      `;

      const fromMsg = (diff.from && diff.from.message) || '';
      const toMsg = (diff.to && diff.to.message) || '';
      const fromAuthor = (diff.from && diff.from.author) || 'Alexander Panasenko <alex@prod.codes>';
      const toAuthor = (diff.to && diff.to.author) || 'Alexander Panasenko <alex@prod.codes>';
      const fromTime = (diff.from && diff.from.committed_at) ? timeAgo(diff.from.committed_at) : '';
      const toTime = (diff.to && diff.to.committed_at) ? timeAgo(diff.to.committed_at) : '';

      const fields = diff.fields || [];
      if (fields.length > 0) {
        html += `
          <div class="diff-metadata-section">
            <div class="diff-section-title">
              <span>Что изменилось в свойствах (${fields.length})</span>
              <span style="font-size:10px; color:var(--text-muted); font-family:var(--font-mono); text-transform:none;">До (Было) ➔ После (Стало)</span>
            </div>
            <div class="diff-meta-grid">
              ${fields.map(f => {
                const label = f.field.replace(/_/g, ' ');
                const fromVal = f.from || '(пусто)';
                const toVal = f.to || '(пусто)';
                return `
                  <div class="diff-meta-row">
                    <div class="diff-meta-field">${escapeHtml(label)}</div>
                    <div class="diff-meta-before">${escapeHtml(fromVal)}</div>
                    <div class="diff-meta-arrow">➔</div>
                    <div class="diff-meta-after">${escapeHtml(toVal)}</div>
                  </div>
                `;
              }).join('')}
            </div>
          </div>
        `;
      }

      html += `
        <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:8px;">
          <div style="font-size:11px; font-weight:700; text-transform:uppercase; letter-spacing:0.5px; color:var(--text-dim);">
            Текст документа ${lineDiff.hasChanges ? `(${lineDiff.splitRows.filter(r => (r.left && r.left.type === 'del') || (r.right && r.right.type === 'add')).length} строк затронуто)` : '(Без изменений)'}
          </div>
          <div style="font-size:11px; color:var(--text-muted); font-family:var(--font-mono);">
            ${lineDiff.linesA.length} строк ➔ ${lineDiff.linesB.length} строк
          </div>
        </div>
      `;

      if (!lineDiff.hasChanges && fields.length === 0) {
        html += `
          <div style="background:var(--card-subtle); border:1px solid var(--border); border-radius:6px; padding:30px; text-align:center; color:var(--text-muted); font-size:13px;">
            Между ревизиями ${fromSha.slice(0, 7)} и ${toSha.slice(0, 7)} изменений нет — документ и метаданные полностью идентичны.
          </div>
        `;
      } else if (!lineDiff.hasChanges) {
        html += `
          <div style="background:var(--card-subtle); border:1px solid var(--border); border-radius:6px; padding:20px; text-align:center; color:var(--text-muted); font-size:12px; margin-bottom:14px;">
            Текст документа не изменялся в этом коммите (изменились только свойства в блоке выше).
          </div>
        `;
      } else if (viewMode === 'split') {
        html += `
          <div class="diff-split-container">
            <div class="diff-split-col">
              <div class="diff-split-header before">
                <div>
                  <span style="font-weight:700; color:#f87171; margin-right:6px;">ДО (БЫЛО):</span>
                  <span style="font-family:var(--font-mono);">${fromSha.slice(0, 7)}</span>
                  <span style="margin-left:6px; color:var(--text);">${escapeHtml(fromMsg)}</span>
                </div>
                <div style="font-size:10px; color:var(--text-dim);">${escapeHtml(fromAuthor)} • ${fromTime}</div>
              </div>
              <div class="diff-split-content">
                ${lineDiff.splitRows.map(r => {
                  if (!r.left) {
                    return `<div class="diff-split-row diff-row-empty"><div class="diff-gutter"></div><div class="diff-cell"></div></div>`;
                  }
                  const rowClass = r.left.type === 'del' ? 'diff-row-del' : '';
                  const prefix = r.left.type === 'del' ? '- ' : '  ';
                  return `
                    <div class="diff-split-row ${rowClass}">
                      <div class="diff-gutter">${r.left.num}</div>
                      <div class="diff-cell">${escapeHtml(prefix + r.left.text)}</div>
                    </div>
                  `;
                }).join('')}
              </div>
            </div>

            <div class="diff-split-col">
              <div class="diff-split-header after">
                <div>
                  <span style="font-weight:700; color:#34d399; margin-right:6px;">ПОСЛЕ (СТАЛО):</span>
                  <span style="font-family:var(--font-mono);">${toSha.slice(0, 7)}</span>
                  <span style="margin-left:6px; color:var(--text);">${escapeHtml(toMsg)}</span>
                </div>
                <div style="font-size:10px; color:var(--text-dim);">${escapeHtml(toAuthor)} • ${toTime}</div>
              </div>
              <div class="diff-split-content">
                ${lineDiff.splitRows.map(r => {
                  if (!r.right) {
                    return `<div class="diff-split-row diff-row-empty"><div class="diff-gutter"></div><div class="diff-cell"></div></div>`;
                  }
                  const rowClass = r.right.type === 'add' ? 'diff-row-add' : '';
                  const prefix = r.right.type === 'add' ? '+ ' : '  ';
                  return `
                    <div class="diff-split-row ${rowClass}">
                      <div class="diff-gutter">${r.right.num}</div>
                      <div class="diff-cell">${escapeHtml(prefix + r.right.text)}</div>
                    </div>
                  `;
                }).join('')}
              </div>
            </div>
          </div>
        `;
      } else {
        html += `
          <div class="diff-unified-container">
            <div class="diff-split-header" style="background:var(--card-subtle); border-bottom:1px solid var(--border);">
              <span style="font-family:var(--font-mono);">${fromSha.slice(0, 7)} (${fromTime}) ➔ ${toSha.slice(0, 7)} (${toTime})</span>
              <span style="color:var(--text);">${escapeHtml(toMsg)}</span>
            </div>
            ${lineDiff.unifiedRows.map(r => {
              const rowClass = r.type === 'del' ? 'diff-row-del' : (r.type === 'add' ? 'diff-row-add' : '');
              return `
                <div class="diff-unified-row ${rowClass}">
                  <div class="diff-unified-gutters">
                    <div class="diff-unified-num">${r.oldNum || ''}</div>
                    <div class="diff-unified-num">${r.newNum || ''}</div>
                  </div>
                  <div class="diff-unified-mark" style="color:${r.type === 'del' ? '#f87171' : (r.type === 'add' ? '#34d399' : 'var(--text-dim)')};">${r.mark}</div>
                  <div class="diff-unified-text">${escapeHtml(r.text)}</div>
                </div>
              `;
            }).join('')}
          </div>
        `;
      }

      container.innerHTML = html;
    }

    function onDiffRevisionChange() {
      const fromSel = document.getElementById('diffFromSelect');
      const toSel = document.getElementById('diffToSelect');
      if (!fromSel || !toSel || !App.diffCurrentKey) return;
      loadDiff(App.diffCurrentKey, fromSel.value, toSel.value);
    }

    function swapDiffRevisions() {
      if (!App.diffCurrentKey || !App.diffFromSha || !App.diffToSha) return;
      loadDiff(App.diffCurrentKey, App.diffToSha, App.diffFromSha);
    }

    function setDiffViewMode(mode) {
      App.diffViewMode = mode;
      renderCurrentDiff();
    }

    async function openDiffForCommit(key, sha) {
      if (!key) return;
      showToast(`Loading diff for ${sha.slice(0, 7)}...`, 'info');
      const histData = await apiFetch(`/knowledge/${encodeURIComponent(key)}/history`);
      const versions = (histData && histData.versions) || [];
      App.currentHistoryList = versions;

      const idx = versions.findIndex(v => v.sha === sha);
      let fromSha = 'initial';
      let toSha = sha;
      if (idx !== -1 && idx < versions.length - 1) {
        fromSha = versions[idx + 1].sha;
      }

      await selectEntry(key);
      switchDetailTab('diff');
      switchTab('search');
      await loadDiff(key, fromSha, toSha);
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

    async function toggleIncidentInlineHistory(key) {
      const container = document.getElementById(`inc-inline-hist-${key}`);
      if (!container) return;

      if (container.style.display !== 'none') {
        container.style.display = 'none';
        return;
      }

      container.style.display = 'block';
      container.innerHTML = `
        <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:8px;">
          <span style="font-size:11px; font-weight:700; text-transform:uppercase; letter-spacing:0.5px; color:var(--text-dim); display:flex; align-items:center; gap:6px;">
            Git Revision History & Timeline
          </span>
          <button class="btn sm" onclick="openHistoryForEntry(${jsArg(key)})">Full History View →</button>
        </div>
        <div style="color:var(--text-muted); font-size:12px; padding:8px 0;">Loading git revisions...</div>
      `;

      try {
        const histData = await apiFetch(`/knowledge/${encodeURIComponent(key)}/history`);
        const versions = (histData && histData.versions) || [];
        if (!versions.length) {
          container.innerHTML = '<div style="color:var(--text-muted); font-size:12px; padding:6px 0;">No revision history recorded for this incident.</div>';
          return;
        }

        container.innerHTML = `
          <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:8px;">
            <span style="font-size:11px; font-weight:700; text-transform:uppercase; letter-spacing:0.5px; color:var(--text-dim); display:flex; align-items:center; gap:6px;">
              Git Revision History (${versions.length} revision${versions.length === 1 ? '' : 's'})
            </span>
            <div style="display:flex; gap:6px;">
              <button class="btn sm" onclick="openDiffForEntry(${jsArg(key)})">Compare Diff</button>
              <button class="btn sm primary" onclick="openHistoryForEntry(${jsArg(key)})">Full History View →</button>
            </div>
          </div>
          <div style="display:flex; flex-direction:column; gap:6px;">
            ${versions.map((h, idx) => `
              <div style="display:flex; justify-content:space-between; align-items:center; background:rgba(255,255,255,0.02); border:1px solid rgba(255,255,255,0.05); border-radius:4px; padding:6px 10px; font-size:11.5px;">
                <div style="display:flex; align-items:center; gap:8px;">
                  <span style="font-family:var(--font-mono); font-size:10.5px; color:var(--accent); background:var(--card-subtle); padding:1px 5px; border-radius:3px;">${h.sha.slice(0, 7)}</span>
                  <span style="font-weight:600; color:var(--text);">${escapeHtml(h.message || 'Updated')}</span>
                  <span style="color:var(--text-muted); font-size:11px;">• ${timeAgo(h.committed_at)}</span>
                  <span style="color:var(--text-dim); font-size:10.5px;">(${escapeHtml(h.author || 'Alexander Panasenko <alex@prod.codes>')})</span>
                </div>
                <div style="display:flex; gap:4px; align-items:center;">
                  ${idx < versions.length - 1 ? `
                    <button class="btn sm" onclick="event.stopPropagation(); openDiffForCommit(${jsArg(key)}, ${jsArg(h.sha)})">Diff vs Prev</button>
                  ` : `
                    <button class="btn sm" onclick="event.stopPropagation(); openDiffForCommit(${jsArg(key)}, ${jsArg(h.sha)})">Initial Diff</button>
                  `}
                </div>
              </div>
            `).join('')}
          </div>
        `;
      } catch (err) {
        container.innerHTML = `<div style="color:var(--red); font-size:12px; padding:6px 0;">Failed to load history: ${escapeHtml(err.message || String(err))}</div>`;
      }
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
              <span style="font-family:var(--font-mono); font-size:12px; margin-left:6px;">commit ${sha.slice(0, 7)}</span>
              <span style="font-size:11px; color:var(--text-muted); margin-left:8px;">${escapeHtml(snap.author || '')}</span>
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

