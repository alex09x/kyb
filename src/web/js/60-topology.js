    // ========================================================
    // TAB 4: NEXT-GEN CYBER TOPOLOGY GRAPH (2026 VISUAL ENGINE)
    // ========================================================

    function buildTopology() {
      const nodes = [];
      const links = [];
      const nodeMap = new Map();

      const serviceSet = new Set();
      const hostSet = new Set();

      // 1. Scan entries for services and hosts dynamically
      App.entries.forEach(e => {
        if (e.hosts && Array.isArray(e.hosts)) {
          e.hosts.forEach(h => {
            if (h && typeof h === 'string') hostSet.add(h.toLowerCase().trim());
          });
        }
        if (e.tags && Array.isArray(e.tags)) {
          e.tags.forEach(t => {
            if (typeof t === 'string' && (t.startsWith('host-') || t.startsWith('node-') || t.startsWith('srv-'))) {
              hostSet.add(t.toLowerCase().trim());
            }
          });
        }
      });

      // Scan for services
      App.entries.forEach(e => {
        let svc = e.service;
        if (!svc && e.tags && e.tags.length) {
          const t = e.tags.find(tag => typeof tag === 'string' && !hostSet.has(tag.toLowerCase().trim()));  // a tag an agent wrote as a number must not take the graph down
          if (t) svc = t;
        }
        if (svc) serviceSet.add(svc.toLowerCase().trim());
      });

      // Existing node positions map to prevent jumping on refresh
      const existingPosMap = new Map();
      if (App.graphNodes) {
        App.graphNodes.forEach(n => existingPosMap.set(n.id, { x: n.x, y: n.y, vx: n.vx, vy: n.vy, pinned: n.pinned }));
      }

      // 2. Add Host Hub Nodes (Server Blade Module)
      let hostIdx = 0;
      const hostCount = hostSet.size || 1;
      hostSet.forEach(host => {
        const angle = (hostIdx / hostCount) * 2 * Math.PI;
        const id = 'host:' + host;
        const prev = existingPosMap.get(id);
        const hub = {
          id: id,
          label: host,
          type: 'host',
          service: host,
          shape: 'blade',
          width: 114,
          height: 26,
          radius: 18,
          color: '#a855f7',
          isHub: true,
          count: 0,
          x: prev ? prev.x : 450 + Math.cos(angle) * 320,
          y: prev ? prev.y : 350 + Math.sin(angle) * 240,
          targetX: 450 + Math.cos(angle) * 320,
          targetY: 350 + Math.sin(angle) * 240,
          vx: prev ? prev.vx : 0,
          vy: prev ? prev.vy : 0,
          pinned: prev ? prev.pinned : false
        };
        nodes.push(hub);
        nodeMap.set(hub.id, hub);
        hostIdx++;
      });

      // 3. Add Service Hub Nodes (Glowing Hexagonal Badges)
      let svcIdx = 0;
      const svcCount = serviceSet.size || 1;
      serviceSet.forEach(svc => {
        const angle = (svcIdx / svcCount) * 2 * Math.PI + 0.35;
        const id = 'svc:' + svc;
        const prev = existingPosMap.get(id);
        const hub = {
          id: id,
          label: svc.toUpperCase(),
          type: 'service',
          service: svc,
          shape: 'hexagon',
          radius: 22,
          color: '#38bdf8',
          isHub: true,
          count: 0,
          x: prev ? prev.x : 450 + Math.cos(angle) * 190,
          y: prev ? prev.y : 350 + Math.sin(angle) * 150,
          targetX: 450 + Math.cos(angle) * 190,
          targetY: 350 + Math.sin(angle) * 150,
          vx: prev ? prev.vx : 0,
          vy: prev ? prev.vy : 0,
          pinned: prev ? prev.pinned : false
        };
        nodes.push(hub);
        nodeMap.set(hub.id, hub);
        svcIdx++;
      });

      // 4. Add Entry Nodes (Micro-Card Capsules)
      App.entries.forEach(e => {
        const isInc = e.kind === 'incident';
        const isTask = e.kind === 'task';
        const svc = String(e.service || (e.tags && e.tags.find(t => typeof t === 'string')) || 'system').toLowerCase();
        const prev = existingPosMap.get(e.key);

        let startX, startY;
        if (prev) {
          startX = prev.x;
          startY = prev.y;
        } else {
          const parentHub = nodeMap.get('svc:' + svc);
          const orbitAngle = Math.random() * 2 * Math.PI;
          const orbitDist = 65 + Math.random() * 80;
          startX = parentHub ? parentHub.x + Math.cos(orbitAngle) * orbitDist : 450 + (Math.random() - 0.5) * 360;
          startY = parentHub ? parentHub.y + Math.sin(orbitAngle) * orbitDist : 350 + (Math.random() - 0.5) * 280;
        }

        const labelText = e.title || e.key;
        const node = {
          id: e.key,
          label: labelText,
          type: isInc ? 'incident' : isTask ? 'task' : 'knowledge',
          service: svc,
          shape: 'capsule',
          width: Math.min(135, Math.max(90, labelText.length * 6.5 + 28)),
          height: isInc ? 24 : 22,
          radius: isInc ? 13 : isTask ? 11 : 9,
          color: isInc ? '#ef4444' : isTask ? '#f59e0b' : '#00b3c4',
          isHub: false,
          x: startX,
          y: startY,
          targetX: startX,
          targetY: startY,
          vx: prev ? prev.vx : 0,
          vy: prev ? prev.vy : 0,
          pinned: prev ? prev.pinned : false,
          data: e
        };
        nodes.push(node);
        nodeMap.set(e.key, node);
      });

      // 5. Connect Links & Calculate Satellite Counts
      App.entries.forEach(e => {
        const srcNode = nodeMap.get(e.key);  // step 4 added a node for every entry

        // Connect to Service Hub
        const svc = String(e.service || (e.tags && e.tags.find(t => typeof t === 'string')) || '').toLowerCase();
        if (svc && nodeMap.has('svc:' + svc)) {
          const sHub = nodeMap.get('svc:' + svc);
          links.push({ source: srcNode, target: sHub });
          sHub.count = (sHub.count || 0) + 1;
        }

        // Connect to Host Hub(s)
        const matchedHosts = new Set();
        if (e.hosts && Array.isArray(e.hosts)) {
          e.hosts.forEach(h => {
            if (h && typeof h === 'string') matchedHosts.add(h.toLowerCase().trim());
          });
        }
        if (e.tags && Array.isArray(e.tags)) {
          e.tags.forEach(t => {
            const low = typeof t === 'string' ? t.toLowerCase().trim() : '';
            if (hostSet.has(low)) matchedHosts.add(low);
          });
        }
        matchedHosts.forEach(h => {
          if (nodeMap.has('host:' + h)) {
            const hHub = nodeMap.get('host:' + h);
            links.push({ source: srcNode, target: hHub });
            hHub.count = (hHub.count || 0) + 1;
          }
        });

        // Explicit knowledge links
        if (e.knowledge && Array.isArray(e.knowledge)) {
          e.knowledge.forEach(targetKey => {
            if (nodeMap.has(targetKey)) {
              links.push({ source: srcNode, target: nodeMap.get(targetKey) });
            }
          });
        }

        // Parent task links
        if (e.parent_task && nodeMap.has(e.parent_task)) {
          links.push({ source: srcNode, target: nodeMap.get(e.parent_task) });
        }
      });

      App.graphNodes = nodes;
      App.graphLinks = links;

      const badgeEl = document.getElementById('topoNodeCountBadge');
      if (badgeEl) badgeEl.textContent = `${nodes.length} nodes`;

      calculateLayoutTargets(App.graphLayout || 'orbit');
      initCanvasGraph();

      if (App.pendingGraphNode) {
        const target = App.pendingGraphNode;
        App.pendingGraphNode = null;
        setTimeout(() => focusNodeById(target), 80);
      }
    }

    // Graph Visual Engine State
    let graphAnimId = null;
    let camera = { x: 0, y: 0, zoom: 1 };
    // Read-only view of the camera for tests and the HUD; the state itself stays private.
    function graphCamera() { return { ...camera, targetZoom: (typeof targetCamera !== 'undefined' && targetCamera) ? targetCamera.zoom : camera.zoom }; }
    let targetCamera = null;
    let draggedNode = null;
    let dragStartPos = { x: 0, y: 0 };
    let hoveredNode = null;
    let focusedNode = null;
    let isPanning = false;
    let panStart = { x: 0, y: 0 };
    let didDragOrPan = false;
    let simulationAlpha = 1.0;
    let animTick = 0;
    let topoSearchQuery = '';

    function reheatSimulation(alpha = 0.8) {
      simulationAlpha = Math.max(simulationAlpha, alpha);
    }

    // Layout Perspectives Engine
    function calculateLayoutTargets(mode) {
      const nodes = App.graphNodes || [];
      if (!nodes.length) return;

      const canvas = document.getElementById('topoCanvas');
      const parent = canvas ? canvas.parentElement : null;
      const width = (parent && parent.clientWidth > 0) ? parent.clientWidth : 960;
      const height = (parent && parent.clientHeight > 0) ? parent.clientHeight : 640;
      const cx = width / 2;
      const cy = height / 2;

      const hostNodes = nodes.filter(n => n.type === 'host');
      const svcNodes = nodes.filter(n => n.type === 'service');
      const entryNodes = nodes.filter(n => !n.isHub);

      if (mode === 'radar') {
        // Concentric Radar Rings Perspective
        // Ring 1: Hosts (radius 135)
        hostNodes.forEach((n, i) => {
          const ang = (i / Math.max(1, hostNodes.length)) * 2 * Math.PI - Math.PI / 2;
          n.targetX = cx + Math.cos(ang) * 140;
          n.targetY = cy + Math.sin(ang) * 125;
        });

        // Ring 2: Services (radius 260)
        svcNodes.forEach((n, i) => {
          const ang = (i / Math.max(1, svcNodes.length)) * 2 * Math.PI - Math.PI / 2 + 0.25;
          n.targetX = cx + Math.cos(ang) * 270;
          n.targetY = cy + Math.sin(ang) * 225;
        });

        // Ring 3: Entries placed radially along outer orbit near parent service
        entryNodes.forEach(n => {
          const parentSvc = svcNodes.find(s => s.service === n.service) || svcNodes[0];
          const baseAng = parentSvc ? Math.atan2(parentSvc.targetY - cy, parentSvc.targetX - cx) : 0;
          const spreadAng = baseAng + (Math.sin(n.id.charCodeAt(0) * 3) * 0.45);
          const rDist = 410 + (Math.cos(n.id.charCodeAt(0) * 7) * 45);
          n.targetX = cx + Math.cos(spreadAng) * rDist;
          n.targetY = cy + Math.sin(spreadAng) * (rDist * 0.85);
        });
      } else if (mode === 'hierarchy') {
        // Hierarchical Architectural Tree Perspective
        // Tier 1: Host Blades
        const hostW = width / (hostNodes.length + 1);
        hostNodes.forEach((n, i) => {
          n.targetX = hostW * (i + 1);
          n.targetY = 100;
        });

        // Tier 2: Service Hubs
        const svcW = width / (svcNodes.length + 1);
        svcNodes.forEach((n, i) => {
          n.targetX = svcW * (i + 1);
          n.targetY = 240;
        });

        // Tier 3: Incidents & Tasks
        const incAndTasks = entryNodes.filter(n => n.type === 'incident' || n.type === 'task');
        const itW = width / (incAndTasks.length + 1);
        incAndTasks.forEach((n, i) => {
          n.targetX = itW * (i + 1);
          n.targetY = 400;
        });

        // Tier 4: Knowledge Documentation
        const docs = entryNodes.filter(n => n.type === 'knowledge');
        const docW = width / (docs.length + 1);
        docs.forEach((n, i) => {
          n.targetX = docW * (i + 1);
          n.targetY = 560;
        });
      } else if (mode === 'grid') {
        // Cyber Matrix / Service Grid Perspective
        const cols = 3;
        const colW = (width - 120) / cols;
        const rowH = 220;

        svcNodes.forEach((sNode, sIdx) => {
          const col = sIdx % cols;
          const row = Math.floor(sIdx / cols);
          const cellX = 60 + col * colW + colW / 2;
          const cellY = 90 + row * rowH;

          sNode.targetX = cellX;
          sNode.targetY = cellY;

          // Align service dependent items inside its cell column
          const children = entryNodes.filter(n => n.service === sNode.service);
          children.forEach((cNode, cIdx) => {
            const subRow = Math.floor(cIdx / 2) + 1;
            const subCol = (cIdx % 2 === 0) ? -1 : 1;
            cNode.targetX = cellX + subCol * 70;
            cNode.targetY = cellY + subRow * 36;
          });
        });

        // Place host hubs along bottom shelf
        const hW = width / (hostNodes.length + 1);
        hostNodes.forEach((hNode, i) => {
          hNode.targetX = hW * (i + 1);
          hNode.targetY = Math.max(height - 60, Math.ceil(svcNodes.length / cols) * rowH + 110);
        });
      } else {
        // Orbit Cluster Perspective
        hostNodes.forEach((n, i) => {
          const ang = (i / Math.max(1, hostNodes.length)) * 2 * Math.PI;
          n.targetX = cx + Math.cos(ang) * 330;
          n.targetY = cy + Math.sin(ang) * 250;
        });

        svcNodes.forEach((n, i) => {
          const ang = (i / Math.max(1, svcNodes.length)) * 2 * Math.PI + 0.35;
          n.targetX = cx + Math.cos(ang) * 190;
          n.targetY = cy + Math.sin(ang) * 150;
        });

        entryNodes.forEach(n => {
          const parentSvc = svcNodes.find(s => s.service === n.service);
          if (parentSvc) {
            const ang = (n.id.charCodeAt(0) * 1.3) % (2 * Math.PI);
            const dist = 65 + (n.id.charCodeAt(0) % 70);
            n.targetX = parentSvc.targetX + Math.cos(ang) * dist;
            n.targetY = parentSvc.targetY + Math.sin(ang) * dist;
          }
        });
      }
    }

    function setGraphLayout(mode) {
      App.graphLayout = mode;
      ['Orbit', 'Radar', 'Hierarchy', 'Grid'].forEach(m => {
        const btn = document.getElementById(`layout${m}Btn`);
        if (btn) btn.classList.toggle('active', m.toLowerCase() === mode);
      });

      calculateLayoutTargets(mode);
      reheatSimulation(1.0);
      showToast(`Perspective switched to ${mode.toUpperCase()}`, 'info');
    }

    function unpinAllGraphNodes() {
      (App.graphNodes || []).forEach(n => { n.pinned = false; });
      reheatSimulation(0.9);
      showToast('All nodes unpinned and relaxed', 'info');
    }

    // Next-Gen Canvas Initialization & Animation Loop
    function initCanvasGraph() {
      const canvas = document.getElementById('topoCanvas');
      if (!canvas) return;
      const ctx = canvas.getContext('2d');
      const hud = document.getElementById('nodeHud');

      function checkResize() {
        const parent = canvas.parentElement;
        const w = parent.clientWidth;
        const h = parent.clientHeight;
        if (w > 0 && h > 0) {
          const targetW = Math.floor(w * window.devicePixelRatio);
          const targetH = Math.floor(h * window.devicePixelRatio);
          if (canvas.width !== targetW || canvas.height !== targetH) {
            canvas.width = targetW;
            canvas.height = targetH;
            ctx.setTransform(1, 0, 0, 1, 0, 0);
            ctx.scale(window.devicePixelRatio, window.devicePixelRatio);
          }
        }
      }
      checkResize();

      function step() {
        checkResize();
        const parent = canvas.parentElement;
        const width = (parent && parent.clientWidth > 0) ? parent.clientWidth : 960;
        const height = (parent && parent.clientHeight > 0) ? parent.clientHeight : 640;
        animTick += 0.035;

        // Smooth Camera Lerp
        if (targetCamera) {
          camera.x += (targetCamera.x - camera.x) * 0.14;
          camera.y += (targetCamera.y - camera.y) * 0.14;
          camera.zoom += (targetCamera.zoom - camera.zoom) * 0.14;
          if (Math.abs(targetCamera.x - camera.x) < 0.4 &&
              Math.abs(targetCamera.y - camera.y) < 0.4 &&
              Math.abs(targetCamera.zoom - camera.zoom) < 0.002) {
            camera.x = targetCamera.x;
            camera.y = targetCamera.y;
            camera.zoom = targetCamera.zoom;
            targetCamera = null;
          }
        }

        let visibleNodes = App.graphNodes || [];
        if (App.graphFilter !== 'all') {
          visibleNodes = visibleNodes.filter(n => n.isHub || n.type === App.graphFilter);
        }
        const visibleNodeSet = new Set(visibleNodes);
        const visibleLinks = (App.graphLinks || []).filter(l => visibleNodeSet.has(l.source) && visibleNodeSet.has(l.target));

        // Physics Simulation & Layout Morphing
        if (App.graphPhysics && simulationAlpha > 0.001) {
          const isOrbit = App.graphLayout === 'orbit';

          // Layout Target Attraction
          visibleNodes.forEach(n => {
            if (!n.pinned) {
              const pull = isOrbit ? 0.03 : 0.14;
              n.vx += (n.targetX - n.x) * pull * simulationAlpha;
              n.vy += (n.targetY - n.y) * pull * simulationAlpha;
            }
          });

          // Repulsion & Spring forces in Orbit perspective
          if (isOrbit) {
            for (let i = 0; i < visibleNodes.length; i++) {
              for (let j = i + 1; j < visibleNodes.length; j++) {
                const dx = visibleNodes[j].x - visibleNodes[i].x;
                const dy = visibleNodes[j].y - visibleNodes[i].y;
                const dist = Math.sqrt(dx * dx + dy * dy) || 1;
                const minDist = (visibleNodes[i].isHub || visibleNodes[j].isHub) ? 180 : 90;
                if (dist < minDist) {
                  const rep = Math.min(8, (minDist - dist) / dist * 1.4) * simulationAlpha;
                  const fx = (dx / dist) * rep;
                  const fy = (dy / dist) * rep;
                  if (!visibleNodes[i].pinned) { visibleNodes[i].vx -= fx; visibleNodes[i].vy -= fy; }
                  if (!visibleNodes[j].pinned) { visibleNodes[j].vx += fx; visibleNodes[j].vy += fy; }
                }
              }
            }

            visibleLinks.forEach(l => {
              const dx = l.target.x - l.source.x;
              const dy = l.target.y - l.source.y;
              const dist = Math.sqrt(dx * dx + dy * dy) || 1;
              const idealDist = (l.source.isHub || l.target.isHub) ? 130 : 75;
              const force = Math.max(-6, Math.min(6, (dist - idealDist) * 0.025)) * simulationAlpha;
              const fx = (dx / dist) * force;
              const fy = (dy / dist) * force;
              if (!l.source.pinned) { l.source.vx += fx; l.source.vy += fy; }
              if (!l.target.pinned) { l.target.vx -= fx; l.target.vy -= fy; }
            });
          }

          // Damping and Velocity Integration
          const maxSpeed = 8 * simulationAlpha;
          visibleNodes.forEach(n => {
            if (!n.pinned) {
              n.vx *= 0.82;
              n.vy *= 0.82;
              const spd = Math.hypot(n.vx, n.vy) || 1;
              if (spd > maxSpeed && maxSpeed > 0.001) {
                n.vx = (n.vx / spd) * maxSpeed;
                n.vy = (n.vy / spd) * maxSpeed;
              }
              if (!isNaN(n.vx)) n.x += n.vx;
              if (!isNaN(n.vy)) n.y += n.vy;
            }
          });

          simulationAlpha *= 0.92;
          if (simulationAlpha <= 0.005) {
            simulationAlpha = 0;
            visibleNodes.forEach(n => { n.vx = 0; n.vy = 0; });
          }
        }

        // Active Neighborhood Spotlight
        const activeNode = focusedNode || hoveredNode;
        let connectedNodeSet = null;
        let activeLinks = [];
        if (activeNode) {
          connectedNodeSet = new Set([activeNode]);
          visibleLinks.forEach(l => {
            if (l.source === activeNode) {
              connectedNodeSet.add(l.target);
              activeLinks.push(l);
            } else if (l.target === activeNode) {
              connectedNodeSet.add(l.source);
              activeLinks.push(l);
            }
          });
        }

        // ==========================================
        // RENDER PASS
        // ==========================================
        ctx.save();
        ctx.clearRect(0, 0, width, height);

        // 1. Draw Subtle Dot Matrix Background
        const worldMinX = -camera.x / camera.zoom;
        const worldMinY = -camera.y / camera.zoom;
        const worldMaxX = (width - camera.x) / camera.zoom;
        const worldMaxY = (height - camera.y) / camera.zoom;

        ctx.translate(camera.x, camera.y);
        ctx.scale(camera.zoom, camera.zoom);

        const dotStep = 32;
        const startGridX = Math.floor(worldMinX / dotStep) * dotStep;
        const endGridX = Math.ceil(worldMaxX / dotStep) * dotStep;
        const startGridY = Math.floor(worldMinY / dotStep) * dotStep;
        const endGridY = Math.ceil(worldMaxY / dotStep) * dotStep;

        ctx.fillStyle = 'rgba(255, 255, 255, 0.05)';
        for (let gx = startGridX; gx <= endGridX; gx += dotStep) {
          for (let gy = startGridY; gy <= endGridY; gy += dotStep) {
            ctx.fillRect(gx, gy, 1.2, 1.2);
          }
        }

        // 2. Draw Clean Bezier Connectors
        visibleLinks.forEach(l => {
          const isActive = activeNode && (l.source === activeNode || l.target === activeNode);
          const cpX = (l.source.x + l.target.x) / 2;
          const cpY = (l.source.y + l.target.y) / 2;
          l._cpX = cpX;
          l._cpY = cpY;

          ctx.beginPath();
          ctx.moveTo(l.source.x, l.source.y);
          ctx.quadraticCurveTo(cpX, cpY, l.target.x, l.target.y);

          if (activeNode) {
            if (isActive) {
              ctx.strokeStyle = '#38bdf8';
              ctx.lineWidth = 1.8;
            } else {
              ctx.strokeStyle = 'rgba(255, 255, 255, 0.025)';
              ctx.lineWidth = 0.6;
            }
          } else {
            const isHubLink = l.target.isHub || l.source.isHub;
            ctx.strokeStyle = isHubLink ? 'rgba(148, 163, 184, 0.16)' : 'rgba(148, 163, 184, 0.08)';
            ctx.lineWidth = isHubLink ? 1.0 : 0.7;
          }
          ctx.stroke();
        });

        // 3. Draw Nodes (Clean modern cards)
        visibleNodes.forEach(n => {
          const isCurrentActive = n === activeNode;
          const isConnected = connectedNodeSet ? connectedNodeSet.has(n) : true;
          const isSearchMatch = topoSearchQuery && (n.label.toLowerCase().includes(topoSearchQuery) || n.id.toLowerCase().includes(topoSearchQuery));

          let alpha = 1.0;
          if (activeNode) alpha = isConnected ? 1.0 : 0.15;
          if (topoSearchQuery) alpha = isSearchMatch ? 1.0 : Math.min(alpha, 0.2);
          ctx.globalAlpha = alpha;

          const isHighlighted = isCurrentActive || isSearchMatch;

          if (n.shape === 'hexagon' || n.type === 'service') {
            // Service Card (width: 140, height: 36)
            const w = 140;
            const h = 36;
            const rx = n.x - w / 2;
            const ry = n.y - h / 2;
            const rad = 6;

            ctx.beginPath();
            ctx.roundRect(rx, ry, w, h, rad);
            ctx.fillStyle = isHighlighted ? '#1e293b' : '#0f172a';
            ctx.fill();

            ctx.strokeStyle = isHighlighted ? '#38bdf8' : 'rgba(56, 189, 248, 0.25)';
            ctx.lineWidth = isHighlighted ? 1.8 : 1.0;
            ctx.stroke();

            // Service Dot
            ctx.beginPath();
            ctx.arc(rx + 12, n.y, 4, 0, Math.PI * 2);
            ctx.fillStyle = '#38bdf8';
            ctx.fill();

            // Service Name
            ctx.fillStyle = isHighlighted ? '#ffffff' : '#f1f5f9';
            ctx.font = 'bold 11px ui-monospace, sans-serif';
            ctx.textAlign = 'left';
            ctx.textBaseline = 'middle';
            ctx.fillText(n.label.slice(0, 12), rx + 22, n.y);

            // Count Badge
            if (n.count !== undefined) {
              const countText = String(n.count);
              ctx.font = '600 10px -apple-system, sans-serif';
              const badgeW = Math.max(16, ctx.measureText(countText).width + 8);
              const bx = rx + w - badgeW - 8;
              const by = n.y - 8;
              ctx.beginPath();
              ctx.roundRect(bx, by, badgeW, 16, 8);
              ctx.fillStyle = 'rgba(255, 255, 255, 0.08)';
              ctx.fill();
              ctx.fillStyle = '#94a3b8';
              ctx.textAlign = 'center';
              ctx.fillText(countText, bx + badgeW / 2, n.y);
            }

          } else if (n.shape === 'blade' || n.type === 'host') {
            // Host Node Card (width: 130, height: 30)
            const w = 130;
            const h = 30;
            const rx = n.x - w / 2;
            const ry = n.y - h / 2;
            const rad = 6;

            ctx.beginPath();
            ctx.roundRect(rx, ry, w, h, rad);
            ctx.fillStyle = isHighlighted ? '#1e293b' : '#0f172a';
            ctx.fill();

            ctx.strokeStyle = isHighlighted ? '#a78bfa' : 'rgba(167, 139, 250, 0.25)';
            ctx.lineWidth = isHighlighted ? 1.8 : 1.0;
            ctx.stroke();

            // Host Dot
            ctx.beginPath();
            ctx.arc(rx + 12, n.y, 3.5, 0, Math.PI * 2);
            ctx.fillStyle = '#a78bfa';
            ctx.fill();

            // Host Label
            ctx.fillStyle = isHighlighted ? '#ffffff' : '#e2e8f0';
            ctx.font = '10.5px ui-monospace, sans-serif';
            ctx.textAlign = 'left';
            ctx.textBaseline = 'middle';
            ctx.fillText(n.label.slice(0, 14), rx + 22, n.y);

          } else {
            // Entry Capsule (width: ~110-140, height: 24)
            const w = Math.min(140, Math.max(90, (n.label || '').length * 6 + 30));
            const h = 24;
            const rx = n.x - w / 2;
            const ry = n.y - h / 2;
            const rad = 5;

            ctx.beginPath();
            ctx.roundRect(rx, ry, w, h, rad);
            ctx.fillStyle = isHighlighted ? '#1e293b' : '#090d16';
            ctx.fill();

            const isInc = n.type === 'incident';
            const isTask = n.type === 'task';
            const dotColor = isInc ? '#f43f5e' : isTask ? '#fbbf24' : '#94a3b8';

            ctx.strokeStyle = isHighlighted ? '#38bdf8' : (isInc ? 'rgba(244, 63, 94, 0.3)' : 'rgba(255, 255, 255, 0.08)');
            ctx.lineWidth = 1.0;
            ctx.stroke();

            // Left Kind Dot
            ctx.beginPath();
            ctx.arc(rx + 9, n.y, 3, 0, Math.PI * 2);
            ctx.fillStyle = dotColor;
            ctx.fill();

            // Label
            ctx.fillStyle = isHighlighted ? '#ffffff' : '#cbd5e1';
            ctx.font = '10px -apple-system, sans-serif';
            ctx.textAlign = 'left';
            ctx.textBaseline = 'middle';
            ctx.fillText((n.label || '').slice(0, 16), rx + 18, n.y);
          }

          // Pinned indicator
          if (n.pinned) {
            ctx.beginPath();
            ctx.arc(n.x + (n.width ? n.width / 2 - 8 : 28), n.y - (n.height ? n.height / 2 - 8 : 10), 3.5, 0, Math.PI * 2);
            ctx.fillStyle = '#f59e0b';
            ctx.fill();
          }
        });

        // 4. Subtle Clean Minimap (Bottom-Left)
        ctx.restore(); // Restore to screen coordinates

        const mmW = 140;
        const mmH = 85;
        const mmX = 16;
        const mmY = height - mmH - 16;

        ctx.beginPath();
        ctx.roundRect(mmX, mmY, mmW, mmH, 6);
        ctx.fillStyle = 'rgba(15, 23, 42, 0.9)';
        ctx.fill();
        ctx.strokeStyle = 'rgba(255, 255, 255, 0.08)';
        ctx.lineWidth = 1;
        ctx.stroke();

        const worldBounds = getTopologyBounds();
        const bW = Math.max(200, worldBounds.maxX - worldBounds.minX);
        const bH = Math.max(160, worldBounds.maxY - worldBounds.minY);

        visibleNodes.forEach(n => {
          const normX = (n.x - worldBounds.minX) / bW;
          const normY = (n.y - worldBounds.minY) / bH;
          const dotX = mmX + 8 + normX * (mmW - 16);
          const dotY = mmY + 8 + normY * (mmH - 16);

          ctx.beginPath();
          ctx.arc(dotX, dotY, n.isHub ? 2 : 1.2, 0, Math.PI * 2);
          ctx.fillStyle = n.type === 'service' ? '#38bdf8' : n.type === 'host' ? '#a78bfa' : 'rgba(255, 255, 255, 0.3)';
          ctx.fill();
        });

        // Camera viewport on minimap
        const camNormMinX = (worldMinX - worldBounds.minX) / bW;
        const camNormMinY = (worldMinY - worldBounds.minY) / bH;
        const camNormMaxX = (worldMaxX - worldBounds.minX) / bW;
        const camNormMaxY = (worldMaxY - worldBounds.minY) / bH;

        const vpX = Math.max(mmX + 2, Math.min(mmX + mmW - 2, mmX + 8 + camNormMinX * (mmW - 16)));
        const vpY = Math.max(mmY + 2, Math.min(mmY + mmH - 2, mmY + 8 + camNormMinY * (mmH - 16)));
        const vpW = Math.min(mmW - 4, Math.max(8, (camNormMaxX - camNormMinX) * (mmW - 16)));
        const vpH = Math.min(mmH - 4, Math.max(8, (camNormMaxY - camNormMinY) * (mmH - 16)));

        ctx.strokeStyle = 'rgba(56, 189, 248, 0.4)';
        ctx.lineWidth = 1;
        ctx.strokeRect(vpX, vpY, vpW, vpH);

        graphAnimId = requestAnimationFrame(step);
      }

      if (graphAnimId) cancelAnimationFrame(graphAnimId);
      step();

      // Mouse & Graph Pos Helper
      function getGraphPos(e) {
        const rect = canvas.getBoundingClientRect();
        const mx = e.clientX - rect.left;
        const my = e.clientY - rect.top;
        const gx = (mx - camera.x) / camera.zoom;
        const gy = (my - camera.y) / camera.zoom;
        return { mx, my, gx, gy };
      }

      function getTopologyBounds() {
        const nodes = App.graphNodes || [];
        if (!nodes.length) return { minX: -200, maxX: 1100, minY: -200, maxY: 900 };
        let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
        nodes.forEach(n => {
          if (n.x < minX) minX = n.x;
          if (n.x > maxX) maxX = n.x;
          if (n.y < minY) minY = n.y;
          if (n.y > maxY) maxY = n.y;
        });
        const pad = 120;
        return { minX: minX - pad, maxX: maxX + pad, minY: minY - pad, maxY: maxY + pad };
      }

      let mouseDownPos = { x: 0, y: 0 };

      canvas.onmousedown = (e) => {
        const { mx, my, gx, gy } = getGraphPos(e);
        mouseDownPos = { x: mx, y: my };
        didDragOrPan = false;

        // Check Mini-Map Click Navigation
        const parent = canvas.parentElement;
        const width = parent.clientWidth;
        const height = parent.clientHeight;
        const mmW = 150, mmH = 95, mmX = 18, mmY = height - mmH - 18;

        if (mx >= mmX && mx <= mmX + mmW && my >= mmY && my <= mmY + mmH) {
          const bounds = getTopologyBounds();
          const normX = (mx - (mmX + 12)) / (mmW - 24);
          const normY = (my - (mmY + 22)) / (mmH - 32);
          const targetWorldX = bounds.minX + normX * (bounds.maxX - bounds.minX);
          const targetWorldY = bounds.minY + normY * (bounds.maxY - bounds.minY);

          targetCamera = {
            x: width / 2 - targetWorldX * camera.zoom,
            y: height / 2 - targetWorldY * camera.zoom,
            zoom: camera.zoom
          };
          return;
        }

        // Test Node Hit
        let clicked = null;
        for (const n of (App.graphNodes || [])) {
          const hitR = n.width ? Math.max(n.width / 2, n.height / 2) + 6 : n.radius + 12;
          if (Math.hypot(n.x - gx, n.y - gy) <= hitR) {
            clicked = n;
            break;
          }
        }

        if (clicked) {
          draggedNode = clicked;
          dragStartPos = { x: clicked.x, y: clicked.y };
          focusNode(clicked);
          openGraphDrawer(clicked);
        } else {
          isPanning = true;
          panStart = { x: mx - camera.x, y: my - camera.y };
        }
      };

      window.onmousemove = (e) => {
        const { mx, my, gx, gy } = getGraphPos(e);

        if (draggedNode) {
          didDragOrPan = true;
          draggedNode.x = gx;
          draggedNode.y = gy;
          reheatSimulation(0.3);
          return;
        }

        if (isPanning) {
          didDragOrPan = true;
          camera.x = mx - panStart.x;
          camera.y = my - panStart.y;
          return;
        }

        // Hover Detection
        let found = null;
        for (const n of (App.graphNodes || [])) {
          const hitR = n.width ? Math.max(n.width / 2, n.height / 2) + 6 : n.radius + 10;
          if (Math.hypot(n.x - gx, n.y - gy) <= hitR) {
            found = n;
            break;
          }
        }
        hoveredNode = found;
        canvas.style.cursor = found ? 'pointer' : 'grab';  // while panning this line is never reached
      };

      window.onmouseup = () => {
        if (draggedNode) {
          const moved = Math.hypot(draggedNode.x - dragStartPos.x, draggedNode.y - dragStartPos.y);
          if (moved > 10) {
            draggedNode.pinned = true;
            showToast(`Pinned ${draggedNode.label} (Double-click to unpin)`, 'info');
          }
          draggedNode = null;
        }
        isPanning = false;
        canvas.style.cursor = 'grab';
      };

      canvas.onwheel = (e) => {
        e.preventDefault();
        const { mx, my } = getGraphPos(e);
        const factor = e.deltaY < 0 ? 1.15 : 0.85;
        const newZoom = Math.min(3.2, Math.max(0.25, camera.zoom * factor));
        camera.x = mx - (mx - camera.x) * (newZoom / camera.zoom);
        camera.y = my - (my - camera.y) * (newZoom / camera.zoom);
        camera.zoom = newZoom;
      };

      canvas.ondblclick = (e) => {
        const { gx, gy } = getGraphPos(e);
        for (const n of (App.graphNodes || [])) {
          const hitR = n.width ? Math.max(n.width / 2, n.height / 2) + 6 : n.radius + 10;
          if (Math.hypot(n.x - gx, n.y - gy) <= hitR) {
            if (n.pinned) {
              n.pinned = false;
              reheatSimulation(0.6);
              showToast(`Unpinned ${n.label}`, 'info');
            } else if (n.isHub) {
              filterFleetByHub(n.id);
            } else {
              selectEntry(n.id);
              switchTab('search');
            }
            break;
          }
        }
      };
    }

    // Sliding Inspector Drawer
    function openGraphDrawer(node) {
      if (!node) return;
      App.drawerNode = node;
      const drawer = document.getElementById('graphDrawer');
      if (!drawer) return;

      const targetHash = `#/graph/${encodeURIComponent(node.id)}`;
      if (window.location.hash !== targetHash) {
        history.replaceState(null, '', targetHash);
      }
      document.title = `KYB — Graph: ${node.label || node.id}`;

      const isHub = node.isHub;
      const badge = document.getElementById('drawerBadge');
      badge.textContent = isHub ? (node.type === 'host' ? 'HOST BLADE' : 'SERVICE HUB') : node.type.toUpperCase();
      badge.className = `badge badge-${node.type}`;

      document.getElementById('drawerKey').textContent = node.id;
      document.getElementById('drawerTitle').textContent = node.label;

      const metaEl = document.getElementById('drawerMeta');
      metaEl.innerHTML = `
        <span>Type: <strong style="color:#fff;">${node.type.toUpperCase()}</strong></span>
        ${node.service ? `<span>Service: <strong style="color:var(--accent);">${escapeHtml(node.service)}</strong></span>` : ''}
        ${node.pinned ? `<span style="color:var(--amber); font-weight:600;">PINNED</span>` : ''}
      `;

      // Connected infrastructure chips
      const linksEl = document.getElementById('drawerLinks');
      const connNodes = (App.graphLinks || [])
        .filter(l => l.source === node || l.target === node)
        .map(l => l.source === node ? l.target : l.source);

      if (connNodes.length > 0) {
        linksEl.innerHTML = connNodes.map(c => `
          <a href="#/graph/${encodeURIComponent(c.id)}" class="badge badge-tag" style="cursor:pointer; text-decoration:none;" onclick="handleGraphChipClick(event, ${jsArg(c.id)})">
            ${escapeHtml(c.label)}
          </a>
        `).join('');
      } else {
        linksEl.innerHTML = '<span style="font-size:11px; color:var(--text-muted);">No active links detected.</span>';
      }

      // Markdown preview
      const contentEl = document.getElementById('drawerContent');
      if (node.data && node.data.body) {
        contentEl.innerHTML = renderMarkdown(node.data.body);
      } else if (isHub) {
        contentEl.innerHTML = `
          <p><strong>Fleet Infrastructure Hub:</strong> Coordinates connections, routing, and operational status for the <code>${escapeHtml(node.service)}</code> subsystem.</p>
          <p>Connected components: <strong>${connNodes.length}</strong> active fleet entities.</p>
        `;
      } else {
        contentEl.innerHTML = '<span style="color:var(--text-muted);">Loading document body...</span>';
        apiFetch(`/knowledge/${encodeURIComponent(node.id)}`).then(data => {
          if (data && data.body) contentEl.innerHTML = renderMarkdown(data.body);
        });
      }

      // Actions
      const openFullBtn = document.getElementById('drawerOpenFullBtn');
      const editBtn = document.getElementById('drawerEditBtn');
      const centerBtn = document.getElementById('drawerCenterBtn');
      const copyBtn = document.getElementById('drawerCopyBtn');
      const copyLinkBtn = document.getElementById('drawerCopyLinkBtn');

      if (!isHub) {
        openFullBtn.style.display = 'inline-block';
        openFullBtn.href = `#/knowledge/${encodeURIComponent(node.id)}`;
        openFullBtn.onclick = (e) => handleDrawerOpenFullClick(e, node.id);
        editBtn.style.display = 'inline-block';
        editBtn.onclick = () => { openEditModal(node.id); };
      } else {
        openFullBtn.style.display = 'none';
        editBtn.style.display = 'none';
      }

      centerBtn.onclick = () => focusNode(node);
      copyBtn.onclick = () => copyToClipboard(node.id, 'Key copied to clipboard');
      if (copyLinkBtn) {
        copyLinkBtn.onclick = () => copyDirectLink(`#/graph/${encodeURIComponent(node.id)}`);
      }

      drawer.classList.add('open');
    }

    function closeGraphDrawer() {
      App.pendingGraphNode = null;  // closing is an answer; a rebuild must not reopen it
      const drawer = document.getElementById('graphDrawer');
      if (drawer) drawer.classList.remove('open');
      App.drawerNode = null;
      if (window.location.hash.startsWith('#/graph/')) {
        history.replaceState(null, '', '#/graph');
        document.title = 'KYB — Cyber Topology';
      }
    }

    function focusNode(node) {
      if (!node) return;
      focusedNode = node;
      const canvas = document.getElementById('topoCanvas');
      if (canvas) {
        const parent = canvas.parentElement;
        const width = (parent && parent.clientWidth > 0) ? parent.clientWidth : 960;
        const height = (parent && parent.clientHeight > 0) ? parent.clientHeight : 640;
        const targetZoom = Math.max(1.3, camera.zoom);
        targetCamera = {
          x: width / 2 - node.x * targetZoom,
          y: height / 2 - node.y * targetZoom,
          zoom: targetZoom
        };
      }
      reheatSimulation(0.35);
    }

    function focusNodeById(id) {
      const clean = (id || '').toLowerCase();
      const n = (App.graphNodes || []).find(item => item.id === id || item.id.toLowerCase() === clean || item.label.toLowerCase() === clean);
      if (n) {
        focusNode(n);
        openGraphDrawer(n);
      }
    }

    function filterFleetByHub(hubId) {
      const term = hubId.replace(/^(svc|host):/, '');
      setGlobalSearchQuery(term);
    }

    function setTopoSearch(query) {
      topoSearchQuery = (query || '').trim().toLowerCase();
      const countEl = document.getElementById('topoSearchCount');
      if (!countEl) return;
      if (!topoSearchQuery) {
        countEl.style.display = 'none';
      } else {
        const matches = (App.graphNodes || []).filter(n =>
          n.label.toLowerCase().includes(topoSearchQuery) || n.id.toLowerCase().includes(topoSearchQuery)
        );
        countEl.textContent = `${matches.length} matching node(s)`;
        countEl.style.display = 'block';
        reheatSimulation(0.3);
      }
    }

    function toggleGraphPhysics() {
      App.graphPhysics = true;
      reheatSimulation(1.0);
      showToast('Graph layout stabilized', 'info');
    }

    function fitGraphCamera() {
      const canvas = document.getElementById('topoCanvas');
      if (!canvas) return;
      const parent = canvas.parentElement;
      const width = (parent && parent.clientWidth > 0) ? parent.clientWidth : 960;
      const height = (parent && parent.clientHeight > 0) ? parent.clientHeight : 640;

      let visibleNodes = App.graphNodes || [];
      if (App.graphFilter !== 'all') {
        visibleNodes = visibleNodes.filter(n => n.isHub || n.type === App.graphFilter);
      }
      if (!visibleNodes.length) return;

      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      visibleNodes.forEach(n => {
        if (n.x < minX) minX = n.x;
        if (n.x > maxX) maxX = n.x;
        if (n.y < minY) minY = n.y;
        if (n.y > maxY) maxY = n.y;
      });

      const pad = 120;
      const bw = Math.max(100, (maxX - minX) + pad * 2);
      const bh = Math.max(100, (maxY - minY) + pad * 2);
      const midX = (minX + maxX) / 2;
      const midY = (minY + maxY) / 2;

      const targetZoom = Math.min(2.0, Math.max(0.35, Math.min(width / bw, height / bh)));
      targetCamera = {
        x: width / 2 - midX * targetZoom,
        y: height / 2 - midY * targetZoom,
        zoom: targetZoom
      };
      showToast('Centered topology view', 'info');
    }

    function zoomGraph(factor) {
      const newZoom = Math.min(3.2, Math.max(0.25, camera.zoom * factor));
      const canvas = document.getElementById('topoCanvas');
      const parent = canvas ? canvas.parentElement : null;
      const width = (parent && parent.clientWidth > 0) ? parent.clientWidth : 960;
      const height = (parent && parent.clientHeight > 0) ? parent.clientHeight : 640;
      targetCamera = {
        x: (width / 2) - ((width / 2) - camera.x) * (newZoom / camera.zoom),
        y: (height / 2) - ((height / 2) - camera.y) * (newZoom / camera.zoom),
        zoom: newZoom
      };
    }

    function resetGraphCamera() {
      fitGraphCamera();
    }

    function setGraphFilter(filter) {
      App.graphFilter = filter;
      ['All', 'Knowledge', 'Incidents', 'Tasks'].forEach(name => {
        const btn = document.getElementById(`gFilter${name}`);
        if (btn) btn.classList.toggle('active', name.toLowerCase() === filter || (filter === 'incident' && name === 'Incidents') || (filter === 'task' && name === 'Tasks'));
      });
      reheatSimulation(0.7);
    }

