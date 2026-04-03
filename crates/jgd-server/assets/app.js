// app.js — WebSocket client, plot display via SSR SVG, resize, navigation.
(function () {
    'use strict';

    var plotImg = document.getElementById('plot-img');
    var plotContainer = document.getElementById('plot-container');
    var wsStatus = document.getElementById('ws-status');
    var btnPrev = document.getElementById('btn-prev');
    var btnNext = document.getElementById('btn-next');
    var plotInfo = document.getElementById('plot-info');

    // --- Plot tracking ---
    // Each entry is {sessionId, plotIndex}.
    var plotEntries = [];
    var currentIndex = -1;

    // Track per-session plot count so we can assign indices for new pages.
    var sessionPlotCount = {};

    function addOrUpdatePlot(sessionId, plotIndex, isNewPage) {
        if (sessionPlotCount[sessionId] === undefined) {
            // First plot for this session.
            sessionPlotCount[sessionId] = 0;
        }

        if (plotEntries.length === 0
            || !plotEntries.some(function (e) { return e.sessionId === sessionId; })
            || isNewPage) {
            // New session or new page — add entry.
            var idx = (plotIndex !== undefined && plotIndex !== null)
                ? plotIndex
                : sessionPlotCount[sessionId];
            sessionPlotCount[sessionId] = idx + 1;
            plotEntries.push({ sessionId: sessionId, plotIndex: idx });
            currentIndex = plotEntries.length - 1;
        }
        // In all cases, refresh the display.
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
        if (currentIndex < plotEntries.length - 1) {
            currentIndex++;
            updateToolbar();
            fetchAndDisplay();
        }
    }

    function updateToolbar() {
        if (plotEntries.length === 0) {
            plotInfo.textContent = 'No plots';
            btnPrev.disabled = true;
            btnNext.disabled = true;
        } else {
            plotInfo.textContent = (currentIndex + 1) + ' / ' + plotEntries.length;
            btnPrev.disabled = currentIndex <= 0;
            btnNext.disabled = currentIndex >= plotEntries.length - 1;
        }
    }

    // --- SVG display ---
    function fetchAndDisplay() {
        if (currentIndex < 0 || currentIndex >= plotEntries.length) return;
        var entry = plotEntries[currentIndex];
        var w = plotContainer.clientWidth;
        var h = plotContainer.clientHeight;
        var url = '/plots/' + encodeURIComponent(entry.sessionId)
            + '/svg?width=' + w + '&height=' + h
            + '&plot_index=' + entry.plotIndex;
        plotImg.src = url;
    }

    // --- Resize handling ---
    var resizeTimer = null;
    function onResize() {
        clearTimeout(resizeTimer);
        resizeTimer = setTimeout(function () {
            // Re-fetch SVG at new size.
            fetchAndDisplay();
            // Send resize to server so R can re-render.
            if (ws && ws.readyState === WebSocket.OPEN) {
                var msg = {
                    type: 'resize',
                    width: plotContainer.clientWidth,
                    height: plotContainer.clientHeight
                };
                if (currentIndex >= 0 && currentIndex < plotEntries.length) {
                    var entry = plotEntries[currentIndex];
                    msg.sessionId = entry.sessionId;
                    msg.plotIndex = entry.plotIndex;
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
    var ws = null;
    var reconnectDelay = 2000;
    var MAX_RECONNECT_DELAY = 30000;

    function connect() {
        var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
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
                var isNewPage = msg.newPage === true;
                var plotIndex = (msg.plotIndex !== undefined && msg.plotIndex !== null)
                    ? msg.plotIndex
                    : undefined;
                addOrUpdatePlot(msg.plot.sessionId, plotIndex, isNewPage);
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
                var found = plotEntries.some(function (e) {
                    return e.sessionId === p.session_id && e.plotIndex === p.plot_index;
                });
                if (!found) {
                    plotEntries.push({ sessionId: p.session_id, plotIndex: p.plot_index });
                    if (!sessionPlotCount[p.session_id]
                        || sessionPlotCount[p.session_id] <= p.plot_index) {
                        sessionPlotCount[p.session_id] = p.plot_index + 1;
                    }
                }
            });
            if (plotEntries.length > 0) {
                currentIndex = plotEntries.length - 1;
                updateToolbar();
                fetchAndDisplay();
            }
        })
        .catch(function () {});
})();
