/*
 * KYB (Know Your Business) — git-backed knowledge base for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

    // Navigation & Hash Router
    function switchTab(tabName, updateHash = true) {
      App.activeTab = tabName;
      document.querySelectorAll('.nav-tab').forEach(t => {
        t.classList.toggle('active', t.getAttribute('data-tab') === tabName);
      });
      document.querySelectorAll('.tab-content').forEach(c => {
        c.classList.toggle('active', c.id === `tab-${tabName}`);
      });
      if (typeof ledLoadVisible === 'function') ledLoadVisible();

      if (updateHash) {
        let targetHash = `#/${tabName}`;
        if (tabName === 'search' && App.selectedKey) {
          targetHash = `#/knowledge/${encodeURIComponent(App.selectedKey)}`;
        } else if (tabName === 'services' && App.place) {
          targetHash = placeHash(App.place);
        }
        if (window.location.hash !== targetHash) {
          history.pushState(null, '', targetHash);
        }
      }

      if (tabName === 'services') renderPlaces();
    }

    // A tag is a filter, not a search term: only entries carrying exactly this tag.
    function filterByTag(tag) {
      const searchEl = document.getElementById('globalSearch');
      if (searchEl) searchEl.value = '#' + tag;
      App.searchQuery = '';
      App.tagFilter = tag;
      App.isBackendSearchResults = false;
      switchTab('search', false);
      const targetHash = `#/search?tag=${encodeURIComponent(tag)}`;
      if (window.location.hash !== targetHash) {
        history.pushState(null, '', targetHash);
      }
      renderKnowledgeList();
      refreshAll();
    }

    function escapeHtml(str) {
      if (!str) return '';
      return String(str).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
    }

    // A value as a JS string literal for an inline handler. HTML-escaping alone is
    // not enough there: the attribute is decoded before the script is parsed.
    function jsArg(v) {
      return escapeHtml(JSON.stringify(String(v == null ? '' : v)));
    }

