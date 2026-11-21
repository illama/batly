// Batly Stream Deck plugin (SDK v2, HTML/JS): shows a device's battery
// level on a key. Live updates through Batly's /api/events (Server-Sent
// Events); falls back to polling /api/devices every 30 s.
/* global WebSocket, EventSource */
'use strict';

var DEFAULT_URL = 'http://localhost:12321';
var POLL_MS = 30000;
var socket = null;
var keys = {};    // context -> { settings, pressedAt }
var sources = {}; // base URL + token -> { devices, es, timer, retry }

function send(event, context, payload) {
  if (socket && socket.readyState === 1) socket.send(JSON.stringify({ event: event, context: context, payload: payload }));
}

function base(s) { return (s.url || DEFAULT_URL).trim().replace(/\/+$/, ''); }
function withToken(url, s) {
  return s.token ? url + (url.indexOf('?') < 0 ? '?' : '&') + 'token=' + encodeURIComponent(s.token) : url;
}
function sourceKey(s) { return base(s) + '#' + (s.token || ''); }

// ------------------------------------------------------------ data sources

function source(s) {
  var k = sourceKey(s);
  if (!sources[k]) {
    sources[k] = { devices: {}, es: null, timer: null, retry: null, settings: { url: base(s), token: s.token } };
    connect(k);
  }
  return sources[k];
}

function connect(k) {
  var src = sources[k];
  if (!src) return;
  if (typeof EventSource === 'undefined') return startPolling(k);
  var es = new EventSource(withToken(src.settings.url + '/api/events', src.settings));
  src.es = es;
  es.addEventListener('device', function (e) {
    try {
      var d = JSON.parse(e.data);
      if (d.removed) delete src.devices[d.id]; else src.devices[d.id] = d;
      redrawFor(k);
    } catch (x) { /* ignore */ }
  });
  es.onerror = function () {
    // The browser retries by itself while the stream is merely interrupted;
    // a closed stream (route missing, refused) means polling.
    if (es.readyState === 2) { es.close(); src.es = null; startPolling(k); }
    markOffline(k);
  };
}

function startPolling(k) {
  var src = sources[k];
  if (!src || src.timer) return;
  poll(k);
  src.timer = setInterval(function () { poll(k); }, POLL_MS);
  // Try the live stream again in five minutes.
  src.retry = setTimeout(function () {
    if (!sources[k]) return;
    clearInterval(src.timer); src.timer = null; connect(k);
  }, 300000);
}

function poll(k) {
  var src = sources[k];
  if (!src) return;
  fetch(withToken(src.settings.url + '/api/devices', src.settings))
    .then(function (r) { if (!r.ok) throw new Error(r.status); return r.json(); })
    .then(function (list) {
      src.devices = {};
      list.forEach(function (d) { src.devices[d.id] = d; });
      src.error = false;
      redrawFor(k);
    })
    .catch(function () { markOffline(k); });
}

function markOffline(k) {
  var src = sources[k];
  if (!src) return;
  src.error = true;
  redrawFor(k);
}

function dropUnused() {
  Object.keys(sources).forEach(function (k) {
    var used = Object.keys(keys).some(function (c) { return sourceKey(keys[c].settings) === k; });
    if (used) return;
    var src = sources[k];
    if (src.es) src.es.close();
    clearInterval(src.timer); clearTimeout(src.retry);
    delete sources[k];
  });
}

// ------------------------------------------------------------ drawing

function pick(src, want) {
  var list = Object.keys(src.devices).map(function (id) { return src.devices[id]; });
  if (want) {
    return list.filter(function (d) { return d.id === want || d.name === want || d.alias === want; })[0];
  }
  // No device chosen: the one with the lowest level.
  return list.filter(function (d) { return d.has_battery && d.battery_percent != null; })
    .sort(function (a, b) { return a.battery_percent - b.battery_percent; })[0];
}

function redrawFor(k) {
  Object.keys(keys).forEach(function (c) { if (sourceKey(keys[c].settings) === k) draw(c); });
}

function draw(context) {
  var key = keys[context];
  if (!key) return;
  var s = key.settings, src = sources[sourceKey(s)];
  var d = src && pick(src, s.device);
  var c = document.getElementById('c'), g = c.getContext('2d');
  g.clearRect(0, 0, 144, 144);
  g.fillStyle = '#17191d';
  g.fillRect(0, 0, 144, 144);

  var p = d && d.battery_percent != null ? Math.round(d.battery_percent) : null;
  var online = d && d.online && !(src && src.error);
  var fg = online ? '#eceef1' : '#80858d';
  var color = !d ? '#80858d' : d.charging ? '#5aa0ff' : p != null && p <= 15 ? '#ef6161' : p != null && p <= 30 ? '#f0a830' : '#3cc47a';

  // Name.
  if (s.showName !== false) {
    var name = d ? (d.alias || d.name) : (s.device || 'Batly');
    g.fillStyle = '#a4a9b1';
    g.font = '600 19px "Segoe UI", sans-serif';
    g.textAlign = 'center';
    g.textBaseline = 'top';
    while (name.length > 3 && g.measureText(name).width > 132) name = name.slice(0, -2) + '…';
    g.fillText(name, 72, 12);
  }

  // Battery.
  var x = 22, y = 46, w = 92, h = 38;
  g.strokeStyle = fg; g.lineWidth = 5;
  g.strokeRect(x, y, w, h);
  g.fillStyle = fg; g.fillRect(x + w + 3, y + 11, 7, 16);
  if (p != null) {
    g.fillStyle = online ? color : '#5b6068';
    g.fillRect(x + 6, y + 6, Math.max(2, (w - 12) * p / 100), h - 12);
  }
  if (d && d.charging) {
    g.fillStyle = '#ffffff';
    g.beginPath();
    g.moveTo(72, y + 3); g.lineTo(60, y + 22); g.lineTo(70, y + 22); g.lineTo(66, y + h - 3);
    g.lineTo(84, y + 15); g.lineTo(74, y + 15); g.closePath(); g.fill();
  }

  // Level.
  g.fillStyle = fg;
  g.font = '700 34px "Segoe UI", sans-serif';
  g.textAlign = 'center';
  g.textBaseline = 'alphabetic';
  g.fillText(!d ? '?' : p == null ? '–' : p + '%', 72, 130);

  send('setImage', context, { image: c.toDataURL('image/png'), target: 0 });
}

// ------------------------------------------------------------ Stream Deck

function onMessage(msg) {
  var m = JSON.parse(msg.data), c = m.context;
  switch (m.event) {
    case 'willAppear':
    case 'didReceiveSettings':
      keys[c] = { settings: (m.payload && m.payload.settings) || {} };
      source(keys[c].settings);
      dropUnused();
      draw(c);
      break;
    case 'willDisappear':
      delete keys[c];
      dropUnused();
      break;
    case 'keyDown':
      if (keys[c]) keys[c].pressedAt = Date.now();
      break;
    case 'keyUp':
      if (!keys[c]) break;
      if (Date.now() - (keys[c].pressedAt || 0) > 600) {
        send('openUrl', null, { url: withToken(base(keys[c].settings) + '/', keys[c].settings) });
      } else {
        poll(sourceKey(keys[c].settings));
        send('showOk', c);
      }
      break;
  }
}

// Called by the Stream Deck software.
// eslint-disable-next-line no-unused-vars
function connectElgatoStreamDeckSocket(port, uuid, registerEvent) {
  socket = new WebSocket('ws://127.0.0.1:' + port);
  socket.onopen = function () { socket.send(JSON.stringify({ event: registerEvent, uuid: uuid })); };
  socket.onmessage = onMessage;
}
