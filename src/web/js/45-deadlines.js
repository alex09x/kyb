    /*
     * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
     * Copyright (c) 2026 Alexander Panasenko
     *
     * Contact: alex@prod.codes
     * Author: https://prod.codes/about/
     * Project: https://github.com/alex09x/kyb
     * SPDX-License-Identifier: MIT
     */

    // Deadlines in the console (design: KYB Deadlines - 1a the strip, 1b the queue and the due
    // column, 1c the by-date view, 2a/2b the card callouts, 2c the form). What is late and what
    // is near is the server's answer - the block on every read and GET /deadlines - and the
    // console only draws it, so the browser and the CLI never disagree about a date.

    // "Today" is this browser's calendar date, sent the way the CLI sends its own.
    function localToday(offsetDays = 0) { return new Date(Date.now() + offsetDays * 86400e3).toLocaleDateString('en-CA'); }

    function dlItem(key) { return App.deadlines.get(key); }
    function dlSurfaced(it) { return !!it && (it.overdue || it.days_left <= it.remind_days); }
    function dlDate(s, opts) { return new Date(String(s).slice(0, 10) + 'T12:00:00').toLocaleDateString('en-US', opts || { month: 'short', day: 'numeric' }); }
    function dlShift(s, days) { const d = new Date(String(s).slice(0, 10) + 'T12:00:00'); d.setDate(d.getDate() + days); return d.toLocaleDateString('en-CA'); }

    // "6d late", "late today", "due today", "in 3d"; outside its reminder window the date itself.
    // The long form is for sentences: "2 days late", "today", "in 4 days".
    function dlWhen(it, long) {
      const n = Math.abs(it.days_left), unit = long ? (n === 1 ? ' day' : ' days') : 'd';
      if (it.overdue) return it.days_left < 0 ? `${n}${unit} late` : 'late today';
      if (it.days_left === 0) return long ? 'today' : 'due today';
      return !long && !dlSurfaced(it) ? dlDate(it.due) : `in ${n}${unit}`;
    }

    // One chip for a deadline everywhere: red when late, amber inside its reminder window, grey
    // after it; HARD framed, a provisional date marked with a tilde.
    function dueChip(it, long) {
      if (!it) return '';
      const word = it.provisional ? 'expected' : 'due';
      const text = long ? `${word} ${dlDate(it.due)} · ${dlWhen(it, true)}` : dlWhen(it, false);
      const tone = it.overdue ? 'late' : dlSurfaced(it) ? 'soon' : 'later';
      return `<span class="due ${tone}" title="${word} ${escapeHtml(it.due)}${it.hard ? ' · hard' : ''}">${it.hard ? '<span class="hard">HARD</span>' : ''}${it.provisional ? '<span class="tilde">~</span>' : ''}${escapeHtml(text)}</span>`;
    }

    // 1a. The strip under the header, on every tab: the block every read answer carries, as the
    // CLI prints it - late first. Absent when nothing is late or near, hidden until tomorrow on ×.
    function renderDeadlineStrip() {
      const el = document.getElementById('deadlineStrip');
      const b = App.deadlineBlock;
      let hiddenToday = false;
      try { hiddenToday = localStorage.getItem('kyb.deadlines.hidden') === localToday(); } catch (_) {}
      if (!b || !b.count || hiddenToday) { el.hidden = true; el.innerHTML = ''; return; }
      const soon = b.count - b.overdue;
      el.hidden = false;
      el.innerHTML = `<span class="dl-sum">${b.overdue ? `<span class="dl-dot"></span><b>${b.overdue} late</b>` : ''}${b.overdue && soon ? '<span class="dl-sep">·</span>' : ''}${soon ? `<span>${soon} due soon</span>` : ''}</span>
        <span class="dl-items">${b.items.map(it => `<a class="dl-chip ${it.overdue ? 'late' : ''}" href="#/knowledge/${encodeURIComponent(it.key)}" onclick="handleCardClick(event, ${jsArg(it.key)}, 'knowledge')">${dueChip(it)}<span class="dl-t">${escapeHtml(it.title)}</span></a>`).join('')}</span>
        <a class="dl-all" href="#/tasks" onclick="event.preventDefault(); showAllDeadlines()">${b.more ? `+${b.more} · ` : ''}all deadlines &rarr;</a>
        <button type="button" class="dl-x" title="Hide until tomorrow" aria-label="Hide deadlines until tomorrow" onclick="hideDeadlineStrip()">&times;</button>`;
    }

    function hideDeadlineStrip() {
      try { localStorage.setItem('kyb.deadlines.hidden', localToday()); } catch (_) {}
      renderDeadlineStrip();
    }

    function showAllDeadlines() {
      Object.assign(App, { taskQueue: 'due', taskSort: 'due', taskStatusFilter: 'live', taskCursor: 0, taskShown: TASK_PAGE });
      switchTab('tasks');
      renderTasks();
    }

    function taskSetSort(sort) { App.taskSort = sort; App.taskCursor = 0; renderTasks(); }

    // 1c. By due date: what has a date first, soonest first (late ones lead), the rest after.
    function dlSort(list) {
      // on its own day a late one ("late today") still comes before one merely due today
      const at = t => { const it = dlItem(t.key); return it ? it.days_left - Number(!!it.overdue) * 0.5 : Infinity; };
      const hard = t => Number(!!dlItem(t.key)?.hard);
      return list.map((t, i) => [t, i]).sort(([a, i], [b, j]) => at(a) - at(b) || hard(b) - hard(a) || i - j).map(([t]) => t);
    }
    const DL_GROUPS = { late: 'Late', week: 'This week', later: 'Later', none: 'No date' };
    function dlGroup(t) {
      const it = dlItem(t.key);
      return !it ? 'none' : it.overdue ? 'late' : it.days_left <= 7 ? 'week' : 'later';
    }
    // The heading in front of the first row of each group, with the group's size in the whole list.
    function dlGroupHead(list, idx) {
      const g = dlGroup(list[idx]);
      if (idx > 0 && dlGroup(list[idx - 1]) === g) return '';
      return `<div class="dl-grp ${g}"><b>${DL_GROUPS[g]}</b><span>${list.filter(t => dlGroup(t) === g).length}</span></div>`;
    }

    // The ribbon over the by-date view: the next 30 days on one axis, the past in red, a dot per
    // deadline (square = hard, dashed = provisional). A dot pressed puts the cursor on its row.
    function dlTimeline(list) {
      const dated = list.map((t, idx) => [dlItem(t.key), idx]).filter(([it]) => it);
      if (!dated.length) return '';
      const start = Math.max(-14, Math.min(-3, ...dated.map(([it]) => it.days_left))), end = 30;
      const pos = d => ((Math.min(Math.max(d, start), end) - start) / (end - start) * 100).toFixed(2) + '%';
      const beyond = dated.filter(([it]) => it.days_left > end).length;
      const ticks = [start, 7, 14, 21, end].map(d => `<span class="dl-tick ${d === end ? 'end' : ''}" style="left:${pos(d)}">${dlDate(localToday(d))}</span>`).join('');
      const marks = dated.filter(([it]) => it.days_left <= end).map(([it, idx]) =>
        `<button type="button" class="dl-m ${it.overdue ? 'late' : dlSurfaced(it) ? 'soon' : ''} ${it.hard ? 'hard' : ''} ${it.provisional ? 'prov' : ''}" style="left:${pos(it.days_left)}" title="${escapeHtml(it.title)} · ${escapeHtml(dlWhen(it, true))}" onclick="App.taskCursor=${idx}; renderTasks();"></button>`).join('');
      return `<div class="dl-tl"><div class="dl-axis"><div class="dl-line"></div><div class="dl-past" style="width:${pos(0)}"></div>
          <span class="dl-today" style="left:${pos(0)}">today &middot; ${dlDate(localToday())}</span>${ticks}${marks}</div>
        <div class="dl-legend"><span><i></i>due</span><span><i class="sq"></i>hard</span><span><i class="dash"></i>expected</span>${beyond ? `<span class="r">+${beyond} beyond 30 days</span>` : ''}</div></div>`;
    }

    // A row's note for its deadline, in the place a blocked reason takes: late, or waiting on an event.
    function dlNote(it) {
      if (it.overdue) return `<div class="inc-note late"><b>${escapeHtml(dlWhen(it, true))}</b> &middot; due ${dlDate(it.due)}</div>`;
      return `<div class="inc-note prov"><b>Waits on</b> &middot; ${escapeHtml(it.waits_on || 'an event')} &mdash; expected ${dlDate(it.due)}</div>`;
    }

    // 2a / 2b. On the card: late says by how much and offers the two things that make sense now;
    // a provisional date says what it waits on and how to set the real one.
    function dlCallout(e, it) {
      const K = jsArg(e.key), T = jsArg(e.title);
      if (it.overdue) {
        const since = it.remind_days ? ` Reminders started ${dlDate(dlShift(it.due, -it.remind_days))} (${it.remind_days} days ahead).` : '';
        return `<div class="callout late"><b>${escapeHtml(dlWhen(it, true))}</b><span>${it.hard ? 'Hard deadline: due' : 'Due'} ${dlDate(it.due, { weekday: 'long', month: 'short', day: 'numeric' })}.${since}</span><button class="btn sm" data-write onclick="moveTaskDate(${K})">Move date</button><button class="btn sm success" data-write onclick="openResolveTaskModal(${K}, ${T})">Done</button></div>`;
      }
      return it.provisional
        ? `<div class="callout"><b>Waits on</b><span>${escapeHtml(it.waits_on || 'an event')}. Until then reminders run on the expected date, ${dlDate(it.due)}.</span><button class="btn sm" data-write onclick="moveTaskDate(${K})">It happened &mdash; set the date</button></div>`
        : '';
    }

    async function moveTaskDate(key) {
      await openEditModal(key);
      const el = document.getElementById('editDue');
      if (el) { el.focus(); el.select(); }
    }

    // 2c. The deadline in the task forms: the date, hard, how early to remind; an event the date
    // depends on behind a checkbox.
    function deadlineFields(p, e) {
      const ev = !!(e.waits_on || e.expected);
      return `<div class="form-row dl-form">
          ${field('Due', `<input type="text" class="form-input mono" id="${p}Due" value="${escapeHtml(e.due || '')}" placeholder="${localToday()}">`, 'A date, or a date and time with its zone.')}
          ${field('Hard', `<label class="dl-tog"><input type="checkbox" id="${p}Hard" ${e.hard ? 'checked' : ''}><span>expiry, contract, regulator</span></label>`)}
          ${field('Remind', `<input type="number" min="0" max="366" class="form-input" id="${p}Remind" value="${e.remind_days ?? ''}" placeholder="14">`, 'Days ahead. Default 14, hard 30.')}
        </div>
        <label class="dl-event"><input type="checkbox" id="${p}Event" ${ev ? 'checked' : ''} onchange="toggleDeadlineEvent('${p}')"> The date depends on an event</label>
        <div class="form-row dl-ev" id="${p}EventFields" ${ev ? '' : 'hidden'}>
          ${field('Waits on', `<input type="text" class="form-input" id="${p}Waits" value="${escapeHtml(e.waits_on || '')}" placeholder="the vendor's quote arrives">`)}
          ${field('Expected', `<input type="text" class="form-input mono" id="${p}Expected" value="${escapeHtml(e.expected || '')}" placeholder="${localToday(7)}">`, 'Reminders run on this until the due date is set.')}
        </div>`;
    }

    function toggleDeadlineEvent(p) { document.getElementById(`${p}EventFields`).hidden = !document.getElementById(`${p}Event`).checked; }

    // Every field goes out, set or cleared: the server keeps what a write leaves out, and the form
    // shows all of them, so what it shows is what is saved.
    function deadlinePayload(p) {
      const v = id => document.getElementById(p + id).value.trim();
      const ev = document.getElementById(`${p}Event`).checked;
      return { due: v('Due'), hard: document.getElementById(`${p}Hard`).checked, remind_days: v('Remind') === '' ? null : Number(v('Remind')),
               waits_on: ev ? v('Waits') : '', expected: ev ? v('Expected') : '' };
    }
