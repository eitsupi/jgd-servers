// app.js — WebSocket client, plot display via SSR SVG, resize, navigation.
(function () {
    'use strict';

    const plotImg = document.getElementById('plot-img');
    const plotContainer = document.getElementById('plot-container');
    const wsStatus = document.getElementById('ws-status');
    const btnPrev = document.getElementById('btn-prev');
    const btnNext = document.getElementById('btn-next');
    const plotInfo = document.getElementById('plot-info');

    // --- Plot tracking ---
    // Each session has an array of plot snapshots (session_id values).
    // For simplicity, we track a single "current session" and an index.
    let sessions = [];       // ordered list of session_ids that have plots
    let currentIndex = -1;

    function addSession(sessionId) {
        if (!sessions.includes(sessionId)) {
            sessions.push(sessionId);
        }
        // Always navigate to the latest.
        currentIndex = sessions.length - 1;
        updateToolbar();
        fetchAndDisplay();
    }

    function navigatePrev() {
        if (currentIndex > 0) {
            currentIndex--;
            updateToolbar();
            fetchAndDisplay();
        }
    }

    function navigateNext() {
        if (currentIndex < sessions.length - 1) {
            currentIndex++;
            updateToolbar();
            fetchAndDisplay();
        }
    }

    function updateToolbar() {
        if (sessions.length === 0) {
            plotInfo.textContent = 'No plots';
            btnPrev.disabled = true;
            btnNext.disabled = true;
        } else {
            plotInfo.textContent = (currentIndex + 1) + ' / ' + sessions.length;
            btnPrev.disabled = currentIndex <= 0;
            btnNext.disabled = currentIndex >= sessions.length - 1;
        }
    }

    // --- SVG display ---
    function fetchAndDisplay() {
        if (currentIndex < 0 || currentIndex >= sessions.length) return;
        const sid = sessions[currentIndex];
        const w = plotContainer.clientWidth;
        const h = plotContainer.clientHeight;
        const url = '/plots/' + encodeURIComponent(sid) + '/svg?width=' + w + '&height=' + h;
        plotImg.src = url;
    }

    // --- Resize handling ---
    let resizeTimer = null;
    function onResize() {
        clearTimeout(resizeTimer);
        resizeTimer = setTimeout(function () {
            // Re-fetch SVG at new size.
            fetchAndDisplay();
            // Send resize to server so R can re-render.
            if (ws && ws.readyState === WebSocket.OPEN) {
                const msg = {
                    type: 'resize',
                    width: plotContainer.clientWidth,
                    height: plotContainer.clientHeight
                };
                if (currentIndex >= 0 && currentIndex < sessions.length) {
                    msg.sessionId = sessions[currentIndex];
                }
                ws.send(JSON.stringify(msg));
            }
        }, 300);
    }

    if (typeof ResizeObserver !== 'undefined') {
        new ResizeObserver(onResize).observe(plotContainer);
    } else {
        window.addEventListener('resize', onResize);
    }

    // --- WebSocket ---
    let ws = null;
    let reconnectDelay = 2000;
    const MAX_RECONNECT_DELAY = 30000;

    function connect() {
        const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
        ws = new WebSocket(proto + '//' + location.host + '/ws');

        ws.onopen = function () {
            wsStatus.classList.add('connected');
            wsStatus.title = 'Connected';
            reconnectDelay = 2000;
        };

        ws.onclose = function () {
            wsStatus.classList.remove('connected');
            wsStatus.title = 'Disconnected';
            setTimeout(connect, reconnectDelay);
            reconnectDelay = Math.min(reconnectDelay * 1.5, MAX_RECONNECT_DELAY);
        };

        ws.onmessage = function (evt) {
            var msg;
            try { msg = JSON.parse(evt.data); } catch (e) { return; }

            if (msg.type === 'frame' && msg.plot && msg.plot.sessionId) {
                addSession(msg.plot.sessionId);
            } else if (msg.type === 'close') {
                // Session ended — keep plots visible.
            }
        };
    }

    connect();

    // --- Toolbar events ---
    btnPrev.addEventListener('click', navigatePrev);
    btnNext.addEventListener('click', navigateNext);

    // --- Initial fetch ---
    // On load, check if there are already plots from the REST API.
    fetch('/plots')
        .then(function (r) { return r.json(); })
        .then(function (plots) {
            plots.forEach(function (p) {
                if (!sessions.includes(p.session_id)) {
                    sessions.push(p.session_id);
                }
            });
            if (sessions.length > 0) {
                currentIndex = sessions.length - 1;
                updateToolbar();
                fetchAndDisplay();
            }
        })
        .catch(function () {});
})();
