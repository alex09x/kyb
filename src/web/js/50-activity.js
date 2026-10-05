    // Fleet Activity & Git Canon Changelog Renderer
    // The change ledger. The base is git and every action is a commit, so the
    // honest view is not "current state" but "what moved": who opened, closed or
    // edited what, and which lines the text lost or gained. Agent searches are
    // real activity but not change; they fold into one line between changes.
    const LED_VERB = {
      created: 'created', updated: 'edited', resolved: 'resolved', mitigated: 'mitigated',
      archived: 'archived', retracted: 'retracted', done: 'done', dropped: 'dropped', transitioned: 'moved',
    };
    function ledVerb(ev) {
      if (ev.kind === 'incident' && ev.action === 'created') return ['opened', 'open'];
      if (ev.action === 'resolved' || ev.action === 'done') return [LED_VERB[ev.action], 'res'];
      if (ev.action === 'retracted' || ev.action === 'archived' || ev.action === 'dropped') return [LED_VERB[ev.action], 'retract'];
      return [LED_VERB[ev.action] || ev.action, ''];
    }
    function ledWho(a) { return (a || '').replace(/\s*<[^>]*>/, '').trim() || 'unknown'; }
    function ledDay(ts) {
      const d = new Date(ts), t = new Date(); const y = new Date(); y.setDate(t.getDate() - 1);
      const same = (a, b) => a.toDateString() === b.toDateString();
      const lbl = d.toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
      return same(d, t) ? 'today · ' + lbl : same(d, y) ? 'yesterday · ' + lbl : lbl;
    }
    function ledTime(ts) { return new Date(ts).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hour12: false }); }

    // One diff per commit, keyed by sha: SHA^..SHA is exactly what that commit
    // did to that entry, even when the commit touched other files too.
    App.ledDiffs = App.ledDiffs || new Map();
    async function ledFetchDiffs(events) {
      const want = events.filter(e => e.key && e.action !== 'created' && !App.ledDiffs.has(e.sha)).slice(0, 30);
      if (!want.length) return false;
      await Promise.all(want.map(async e => {
        try {
          const d = await apiFetch(`/knowledge/${encodeURIComponent(e.key)}/diff?from=${e.sha}^&to=${e.sha}`);
          App.ledDiffs.set(e.sha, d || null);
        } catch (_) { App.ledDiffs.set(e.sha, null); }
      }));
      return true;
    }
    function ledDiffHtml(d) {
      if (!d || !d.changed) return '';
      const rows = [];
      for (const f of (d.fields || [])) {
        if (['updated_at'].includes(f.field)) continue;
        rows.push(`<div class="fld">${escapeHtml(f.field)}: <em>${escapeHtml(String(f.from ?? '∅'))}</em> → ${escapeHtml(String(f.to ?? '∅'))}</div>`);
      }
      // The whole change, every time. The point of the ledger is that nothing
      // needs a click to be seen.
      const rm = (d.body?.removed || []).filter(l => l.trim()), ad = (d.body?.added || []).filter(l => l.trim());
      rm.forEach(l => rows.push(`<div class="del">− ${escapeHtml(l)}</div>`));
      ad.forEach(l => rows.push(`<div class="add">+ ${escapeHtml(l)}</div>`));
      return rows.length ? `<div class="led-d">${rows.join('')}</div>` : '';
    }

    function renderFeed() {
      const body = document.getElementById('ledBody');
      if (!body) return;

      let events = (App.activity || []).map(a => ({ ...a, t: new Date(a.ts).getTime() }))
        .filter(a => App.activityFilter === 'all' || a.kind === App.activityFilter)
        .sort((a, b) => b.t - a.t);
      const searches = (App.audit || [])
        .filter(x => x.path === '/search' && x.query && /(^|&)q=/.test(x.query))
        .map(x => {
          // a stray '%' in an agent's query must not take the whole refresh down
          let raw = x.query; try { raw = decodeURIComponent(x.query.replace(/\+/g, ' ')); } catch (_) {}
          const m = raw.match(/(?:^|&)q=([^&]*)/);
          return { t: new Date(x.ts).getTime(), q: (m && m[1] || '').trim(), ip: x.ip, ts: x.ts };
        })
        .filter(x => x.q).sort((a, b) => b.t - a.t);

      // summary line: today's changes by verb
      const today = new Date().toDateString();
      const td = events.filter(e => new Date(e.t).toDateString() === today);
      const n = v => td.filter(e => ledVerb(e)[0] === v).length;
      document.getElementById('ledTop').innerHTML = `
        <b>${td.length}</b><span>changes today</span>
        <span style="color:var(--text-muted)">&middot;</span>
        <span>${n('opened')} opened &middot; ${n('resolved') + n('done')} resolved &middot; ${n('edited')} edited &middot; ${n('created')} created</span>
        <span style="color:var(--text-muted)">&middot; ${searches.filter(x => new Date(x.t).toDateString() === today).length} searches</span>
        <span class="chips">
          ${['all','knowledge','incident','task'].map(k => `<button class="chip ${App.activityFilter === k ? 'active' : ''}" onclick="setActivityFilter('${k}')">${k === 'all' ? 'everything' : k === 'knowledge' ? 'knowledge' : k + 's'}</button>`).join('')}
        </span>`;

      if (!events.length) { body.innerHTML = '<div class="led-empty">No changes recorded yet.</div>'; return; }

      // searches fold between the two changes they happened between
      let html = '', day = null, si = 0;
      const foldSearches = (upper, lower, idx) => {
        const batch = [];
        while (si < searches.length && searches[si].t > lower) { if (searches[si].t <= upper) batch.push(searches[si]); si++; }
        if (!batch.length) return '';
        const open = App.ledOpenFolds && App.ledOpenFolds.has(idx);
        const agents = new Set(batch.map(b => b.ip)).size;
        if (!open) return `<div class="led-fold" onclick="ledToggleFold(${idx})">⋯ ${batch.length} search${batch.length > 1 ? 'es' : ''} by ${agents} agent${agents > 1 ? 's' : ''} · <span class="acc">show</span></div>`;
        return `<div class="led-fold" onclick="ledToggleFold(${idx})">⋯ ${batch.length} searches · <span class="acc">hide</span></div>` +
          batch.map(b => `<div class="led-q"><span class="tm" style="font-family:var(--font-mono)">${ledTime(b.ts)}</span><span></span><span class="qq"><a href="#/search?q=${encodeURIComponent(b.q)}" onclick="handleSearchFeedClick(event, ${jsArg(b.q)})">${escapeHtml(b.q)}</a></span><span>${escapeHtml(b.ip || '')}</span></div>`).join('');
      };

      html += foldSearches(Infinity, events[0].t, 0);
      events.slice(0, 120).forEach((e, i) => {
        const dl = ledDay(e.ts);
        if (dl !== day) { day = dl; html += `<div class="led-day">${dl}</div>`; }
        const [verb, cls] = ledVerb(e);
        const dotCls = cls === 'open' ? 'open' : cls === 'res' ? 'res' : e.kind === 'task' ? 'task' : '';
        const sub = [e.key, e.kind !== 'knowledge' ? e.kind : null].filter(Boolean).join(' · ');
        html += `<div class="led-ev" data-i="${i}">
          <span class="tm">${ledTime(e.ts)}</span>
          <div class="ln"><span class="dot ${dotCls}"></span><i></i></div>
          <div class="what">
            <div class="h"><span class="led-verb ${cls}">${verb}</span><b><a href="#/knowledge/${encodeURIComponent(e.key || '')}" onclick="handleCardClick(event, ${jsArg(e.key || '')}, 'knowledge')">${escapeHtml(e.title || e.key || e.message || '')}</a></b></div>
            <span class="sub">${escapeHtml(sub)}</span>
            ${ledDiffHtml(App.ledDiffs.get(e.sha))}
          </div>
          <span class="who" title="${escapeHtml(e.sha || '')}">${escapeHtml(ledWho(e.author))}</span>
        </div>`;
        const next = events[i + 1];
        html += foldSearches(e.t, next ? next.t : -Infinity, i + 1);
      });
      body.innerHTML = html;

      App.ledEvents = events;
      ledLoadVisible();
    }
    // Diffs are fetched for the rows on screen, not for the whole feed and not
    // while the ledger is hidden: every one of these requests lands in the audit log.
    function ledLoadVisible() {
      const body = document.getElementById('ledBody');
      if (!body || !body.offsetParent || App.ledLoading) return;
      const limit = window.innerHeight + 400;
      const want = [...body.querySelectorAll('.led-ev')].filter(r => { const b = r.getBoundingClientRect(); return b.bottom > -400 && b.top < limit; })
        .map(r => (App.ledEvents || [])[+r.dataset.i]).filter(Boolean);
      App.ledLoading = true;
      ledFetchDiffs(want).then(got => { App.ledLoading = false; if (got) renderFeed(); }, () => { App.ledLoading = false; });
    }
    document.addEventListener('scroll', () => { clearTimeout(App.ledScrollT); App.ledScrollT = setTimeout(ledLoadVisible, 150); }, true);
    function ledToggleFold(idx) {
      App.ledOpenFolds = App.ledOpenFolds || new Set();
      App.ledOpenFolds.has(idx) ? App.ledOpenFolds.delete(idx) : App.ledOpenFolds.add(idx);
      renderFeed();
    }

    function setActivityFilter(filter) {
      App.activityFilter = filter;
      renderFeed();
    }

    function setGlobalSearchQuery(q) {
      const searchEl = document.getElementById('globalSearch');
      if (searchEl) {
        searchEl.value = q;
        App.searchQuery = q;
        App.tagFilter = null;
        switchTab('search', false);
        const targetHash = q ? `#/search?q=${encodeURIComponent(q)}` : '#/search';
        if (window.location.hash !== targetHash) {
          history.pushState(null, '', targetHash);
        }
        renderKnowledgeList();
      }
    }



