    // Keyboard Shortcuts & Events
    window.addEventListener('DOMContentLoaded', () => {
      // Tab navigation
      document.querySelectorAll('.nav-tab').forEach(tab => {
        tab.addEventListener('click', (e) => {
          e.preventDefault();
          switchTab(tab.getAttribute('data-tab'), true);
        });
      });

      // Filter chips
      document.querySelectorAll('.chip[data-filter]').forEach(chip => {
        chip.addEventListener('click', () => {
          document.querySelectorAll('.chip[data-filter]').forEach(c => c.classList.remove('active'));
          chip.classList.add('active');
          App.currentFilter = chip.getAttribute('data-filter');
          renderKnowledgeList();
        });
      });

      // Global search
      const searchEl = document.getElementById('globalSearch');
      searchEl.addEventListener('input', (e) => {
        const v = e.target.value;
        const m = v.match(/^#(\S+)$/);  // "#postgres" filters by tag (any tag the server accepts); anything else is a text search
        App.tagFilter = m ? m[1] : null;
        App.searchQuery = m ? '' : v;
        if (App.activeTab !== 'search') switchTab('search', false);
        renderKnowledgeList();
      });
      searchEl.addEventListener('keydown', async (e) => {
        if (e.key === 'Enter') {
          const q = searchEl.value.trim();
          if (q) {
            const targetHash = `#/search?q=${encodeURIComponent(q)}`;
            if (window.location.hash !== targetHash) {
              history.pushState(null, '', targetHash);
            }
            const data = await apiFetch(`/search?q=${encodeURIComponent(q)}&limit=100`);
            if (data && data.hits) {
              App.isBackendSearchResults = true;
              App.entries = data.hits;
              App.searchQuery = q;
              renderKnowledgeList();
              if (data.hits.length > 0) {
                selectEntry(data.hits[0].key, false);
              }
              showToast(`Found ${data.hits.length} match(es) for "${q}"`, 'info');
            }
          } else {
            App.isBackendSearchResults = false;
            history.pushState(null, '', '#/search');
            refreshAll();
          }
        }
      });

      // Keyboard shortcuts
      window.addEventListener('keydown', (e) => {
        const inInput = ['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName);

        if (e.key === 'Escape') {
          document.querySelectorAll('.modal-backdrop.open').forEach(m => m.classList.remove('open'));
          if (document.activeElement === searchEl) searchEl.blur();
          return;
        }

        if (inInput) return;

        if (e.key === '/') {
          e.preventDefault();
          searchEl.focus();
        } else if (e.key === '1') {
          switchTab('search', true);
        } else if (e.key === '2') {
          switchTab('incidents', true);
        } else if (e.key === '3') {
          switchTab('tasks', true);
        } else if (e.key === '4') {
          switchTab('services', true);
        } else if (e.key === '5') {
          switchTab('feed', true);
        } else if (e.key === 'n' || e.key === 'N') {
          e.preventDefault();
          openNewModal();
        } else if (e.key === 'e' || e.key === 'E') {
          if (App.selectedKey) {
            e.preventDefault();
            openEditModal(App.selectedKey);
          }
        } else if (e.key === 'r' || e.key === 'R') {
          e.preventDefault();
          manualRefresh();
        } else if (e.key === '?') {
          e.preventDefault();
          openShortcutsModal();
        } else if (e.key === 'j' || e.key === 'ArrowDown') {
          e.preventDefault();
          navigateList(1);
        } else if (e.key === 'k' || e.key === 'ArrowUp') {
          e.preventDefault();
          navigateList(-1);
        }
      });

      function navigateList(dir) {
        const list = getFilteredEntries();
        if (!list.length) return;
        const curIdx = list.findIndex(e => e.key === App.selectedKey);
        const nextIdx = Math.max(0, Math.min(list.length - 1, curIdx + dir));
        selectEntry(list[nextIdx].key, true);
        const card = document.querySelector(`.item-card[data-key="${list[nextIdx].key}"]`);
        if (card) card.scrollIntoView({ block: 'nearest' });
      }

      // Initial route, popstate & hashchange listener
      function applyRoute() {
        const fullHash = window.location.hash || '';
        if (!fullHash || fullHash === '#' || fullHash === '#/') {
          if (App.activeTab !== 'search') switchTab('search', false);
          if (!App.selectedKey && App.entries.length > 0) {
            selectEntry(App.entries[0].key, true);
          }
          return;
        }

        const rawPath = fullHash.replace(/^#\/?/, '');
        const [routePath, queryString] = rawPath.split('?');
        const params = new URLSearchParams(queryString || '');
        const segments = routePath.split('/').map(s => decodeURIComponent(s).trim()).filter(Boolean);
        const root = segments[0] || 'search';
        const paramKey = segments[1] || '';

        if (root === 'knowledge' || root === 'doc') {
          const key = paramKey || params.get('key');
          if (App.activeTab !== 'search') switchTab('search', false);
          if (searchEl && searchEl.value) {
            searchEl.value = '';
            App.searchQuery = '';
            App.isBackendSearchResults = false;
            renderKnowledgeList();
          }
          if (key) {
            const rev = params.get('rev') || params.get('at');
            selectEntry(key, false).then(() => {
              if (rev) {
                inspectRevision(key, rev);
              }
            });
            setTimeout(() => {
              try {
                const card = document.querySelector(`.item-card[data-key="${CSS.escape(key)}"]`);
                if (card) {
                  card.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
                  card.classList.add('selected');
                }
              } catch (_) {}
            }, 60);
          }
        } else if (root === 'incidents') {
          const key = paramKey || params.get('key');
          if (App.activeTab !== 'incidents') switchTab('incidents', false);
          if (key) {
            App.pendingHighlight = { tab: 'incidents', key };
            highlightCard('incidents', key);
            document.title = `KYB — Incident: ${key}`;
          } else {
            document.title = 'KYB — Fleet Incidents';
          }
        } else if (root === 'tasks') {
          const key = paramKey || params.get('key');
          if (App.activeTab !== 'tasks') switchTab('tasks', false);
          if (key) {
            App.pendingHighlight = { tab: 'tasks', key };
            highlightCard('tasks', key);
            document.title = `KYB — Task: ${key}`;
          } else {
            document.title = 'KYB — Fleet Tasks';
          }
        } else if (root === 'services' || root === 'hosts' || root === 'graph' || root === 'topology') {
          // #/graph and #/topology are old links to the removed map: they land on the list.
          const name = (root === 'services' || root === 'hosts') ? paramKey : '';
          if (App.activeTab !== 'services') switchTab('services', false);
          if (name) selectPlace(root === 'hosts' ? 'host' : 'service', name, false);
          document.title = name ? `KYB — ${name}` : 'KYB — Services';
        } else if (root === 'search') {
          const q = params.get('q') || params.get('query') || '';
          const tag = params.get('tag') || '';
          const term = q || tag;
          if (App.activeTab !== 'search') switchTab('search', false);
          if (tag && !q) {
            // A tag link reloaded must show what the tag click showed: the tagged entries, not a text search for the word.
            filterByTag(tag);
          } else if (term) {
            App.tagFilter = null;  // a text search replaces whatever tag was filtering before
            if (searchEl) searchEl.value = term;
            App.searchQuery = term;
            apiFetch(`/search?q=${encodeURIComponent(term)}&limit=100`).then(data => {
              if (data && data.hits) {
                App.isBackendSearchResults = true;
                App.entries = data.hits;
                renderKnowledgeList();  // selects the top hit itself when nothing is selected
              } else {
                renderKnowledgeList();
              }
            });
          } else {
            App.tagFilter = null;
            App.isBackendSearchResults = false;
            if (searchEl && searchEl.value.startsWith('#')) searchEl.value = '';
            renderKnowledgeList();
          }
          document.title = term ? `KYB — Search: ${term}` : 'KYB — Fleet Knowledge';
        } else if (root === 'feed' || root === 'activity') {
          if (App.activeTab !== 'feed') switchTab('feed', false);
          document.title = 'KYB — Fleet Operations Feed';
        } else {
          if (App.activeTab !== 'search') switchTab('search', false);
        }
      }

      applyRoute();
      window.addEventListener('hashchange', applyRoute);
      window.addEventListener('popstate', applyRoute);

      // Initial load & 4s polling
      refreshAll().then(() => {
        applyRoute();
      });
      App.refreshTimer = setInterval(refreshAll, 4000);  // kept so the loop can be paused (the UI test drives refreshes itself)
    });
