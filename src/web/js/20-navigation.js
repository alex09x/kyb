    // Deep Link Navigation & Anchor Handlers
    function handleCardClick(event, key, type = 'knowledge') {
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.button === 1) {
        return; // Native browser opens new tab
      }
      event.preventDefault();
      if (type === 'knowledge') {
        selectEntry(key, true);
        if (App.activeTab !== 'search') switchTab('search', false);
      } else if (type === 'incidents') {
        window.location.hash = `#/incidents/${encodeURIComponent(key)}`;
      } else if (type === 'tasks') {
        window.location.hash = `#/tasks/${encodeURIComponent(key)}`;
      }
    }

    function handleTagClick(event, tag) {
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.button === 1) return;
      event.preventDefault();
      filterByTag(tag);
    }

    function handleSearchFeedClick(event, q) {
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.button === 1) return;
      event.preventDefault();
      setGlobalSearchQuery(q);
    }


    function highlightCard(tab, key) {
      if (!key) return false;
      let el = null;
      try {
        const selector = tab === 'incidents' ? `.incident-card[data-key="${CSS.escape(key)}"]` : `.task-card[data-key="${CSS.escape(key)}"]`;
        el = document.querySelector(selector);
      } catch (err) {
        el = document.getElementById(`${tab === 'incidents' ? 'incident' : 'task'}-${key}`);
      }
      if (el) {
        el.scrollIntoView({ behavior: 'smooth', block: 'center' });
        el.classList.remove('target-highlight');
        void el.offsetWidth;
        el.classList.add('target-highlight');
        return true;
      }
      return false;
    }

