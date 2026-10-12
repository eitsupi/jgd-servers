// WebSocket events invalidate an authoritative, ordered REST snapshot.
(function () {
    'use strict';

    var plotImg = document.getElementById('plot-img');
    var plotContainer = document.getElementById('plot-container');
    var wsStatus = document.getElementById('ws-status');
    var btnPrev = document.getElementById('btn-prev');
    var btnNext = document.getElementById('btn-next');
    var plotInfo = document.getElementById('plot-info');
    var plotEntries = [];
    var currentIndex = -1;
    var ws = null;
    var imageEpoch = 0;
    var syncing = false;
    var syncPending = false;
    var syncTimer = null;

    function updateToolbar() {
        if (plotEntries.length === 0) {
            plotInfo.textContent = 'No plots';
            btnPrev.disabled = true;
            btnNext.disabled = true;
            plotImg.removeAttribute('src');
        } else {
            var entry = plotEntries[currentIndex];
            plotInfo.textContent = (currentIndex + 1) + ' / ' + plotEntries.length
                + (entry.connected ? '' : ' (disconnected)');
            btnPrev.disabled = currentIndex <= 0;
            btnNext.disabled = currentIndex >= plotEntries.length - 1;
        }
    }

    function fetchAndDisplay() {
        if (currentIndex < 0 || currentIndex >= plotEntries.length) return;
        var entry = plotEntries[currentIndex];
        var url = '/plots/' + encodeURIComponent(entry.session_id)
            + '/svg?width=' + plotContainer.clientWidth
            + '&height=' + plotContainer.clientHeight
            + '&plot_index=' + entry.plot_index
            + '&revision=' + entry.revision
            + '&plot_id=' + entry.plot_id
            + '&epoch=' + imageEpoch;
        if (plotImg.getAttribute('src') !== url) plotImg.src = url;
    }

    function applySnapshot(plots) {
        var selected = plotEntries[currentIndex];
        var followLatest = currentIndex === plotEntries.length - 1;
        var seen = new Set();
        plotEntries = plots.filter(function (plot) {
            if (seen.has(plot.plot_id)) return false;
            seen.add(plot.plot_id);
            return true;
        });
        currentIndex = selected ? plotEntries.findIndex(function (plot) {
            return plot.plot_id === selected.plot_id;
        }) : -1;
        if (followLatest || currentIndex < 0) currentIndex = plotEntries.length - 1;
        updateToolbar();
        fetchAndDisplay();
    }

    // Never infer history from frame counts: reconnects and lag can miss events.
    // Serialize requests, and repeat if an event arrived while fetching.
    function requestSync() {
        syncPending = true;
        if (syncing || syncTimer !== null) return;
        syncTimer = setTimeout(syncPlots, 0);
    }

    function syncPlots() {
        syncTimer = null;
        syncPending = false;
        syncing = true;
        fetch('/plots', { cache: 'no-store' })
            .then(function (response) {
                if (!response.ok) throw new Error('Plot snapshot failed');
                return response.json();
            })
            .then(function (plots) {
                applySnapshot(plots);
            })
            .catch(function () {
                // Retry transient failures even if no further frame arrives.
                syncTimer = setTimeout(syncPlots, 2000);
            })
            .finally(function () {
                syncing = false;
                if (syncPending && syncTimer === null) requestSync();
            });
    }

    function navigate(delta) {
        var next = currentIndex + delta;
        if (next < 0 || next >= plotEntries.length) return;
        currentIndex = next;
        updateToolbar();
        fetchAndDisplay();
    }

    var resizeTimer = null;
    function onResize() {
        clearTimeout(resizeTimer);
        resizeTimer = setTimeout(function () {
            fetchAndDisplay();
            var entry = plotEntries[currentIndex];
            if (entry && entry.connected && ws && ws.readyState === WebSocket.OPEN) {
                ws.send(JSON.stringify({
                    type: 'resize',
                    width: plotContainer.clientWidth,
                    height: plotContainer.clientHeight,
                    sessionId: entry.session_id,
                    plotIndex: entry.plot_index
                }));
            }
        }, 300);
    }

    if (typeof ResizeObserver !== 'undefined') {
        new ResizeObserver(onResize).observe(plotContainer);
    } else {
        window.addEventListener('resize', onResize);
    }

    var reconnectDelay = 2000;
    var MAX_RECONNECT_DELAY = 30000;
    function connect() {
        var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
        ws = new WebSocket(proto + '//' + location.host + '/ws');
        ws.onopen = function () {
            imageEpoch += 1;
            wsStatus.classList.add('connected');
            wsStatus.title = 'Connected';
            reconnectDelay = 2000;
            requestSync();
        };
        ws.onclose = function () {
            wsStatus.classList.remove('connected');
            wsStatus.title = 'Disconnected';
            setTimeout(connect, reconnectDelay);
            reconnectDelay = Math.min(reconnectDelay * 1.5, MAX_RECONNECT_DELAY);
        };
        ws.onmessage = function (event) {
            var message;
            try { message = JSON.parse(event.data); } catch { return; }
            if (message.type === 'frame' || message.type === 'close') requestSync();
        };
    }

    btnPrev.addEventListener('click', function () { navigate(-1); });
    btnNext.addEventListener('click', function () { navigate(1); });
    updateToolbar();
    connect();
    requestSync();
})();
