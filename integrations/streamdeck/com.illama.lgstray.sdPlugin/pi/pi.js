// Property inspector of the LGSTray action: LGSTray address, token and the
// device to show (list read from /api/devices).
/* global WebSocket */
'use strict';

var socket = null, uuid = null, settings = {};
var $ = function (id) { return document.getElementById(id); };

function save() {
  settings = {
    url: $('url').value.trim(),
    token: $('token').value.trim(),
    device: $('device').value,
    showName: $('showName').checked
  };
  if (socket) socket.send(JSON.stringify({ event: 'setSettings', context: uuid, payload: settings }));
}

function option(value, text) {
  var o = document.createElement('option');
  o.value = value; o.textContent = text;
  return o;
}

function loadDevices() {
  var url = ($('url').value.trim() || 'http://localhost:12321').replace(/\/+$/, '') + '/api/devices';
  var token = $('token').value.trim();
  if (token) url += '?token=' + encodeURIComponent(token);
  var sel = $('device'), status = $('status');
  status.className = 'hint'; status.textContent = '…';
  fetch(url).then(function (r) { if (!r.ok) throw new Error('HTTP ' + r.status); return r.json(); })
    .then(function (list) {
      sel.innerHTML = '';
      sel.appendChild(option('', 'Lowest battery (automatic)'));
      list.filter(function (d) { return d.has_battery; }).forEach(function (d) {
        var label = (d.alias || d.name) + (d.host ? ' (' + d.host + ')' : '') +
          (d.battery_percent != null ? ' – ' + d.battery_percent + '%' : '');
        sel.appendChild(option(d.id, label));
      });
      // Keep a device that is not connected right now.
      if (settings.device && !list.some(function (d) { return d.id === settings.device; })) {
        sel.appendChild(option(settings.device, settings.device + ' (not found)'));
      }
      sel.value = settings.device || '';
      status.textContent = list.length + ' device(s)';
    })
    .catch(function (e) {
      status.className = 'hint error';
      status.textContent = 'Cannot reach LGSTray (' + e.message + '). Is the HTTP server enabled?';
      if (!sel.options.length) sel.appendChild(option(settings.device || '', settings.device || 'Lowest battery (automatic)'));
    });
}

// Called by the Stream Deck software.
// eslint-disable-next-line no-unused-vars
function connectElgatoStreamDeckSocket(port, inUuid, registerEvent, info, actionInfo) {
  uuid = inUuid;
  try { settings = JSON.parse(actionInfo).payload.settings || {}; } catch (e) { settings = {}; }
  $('url').value = settings.url || '';
  $('token').value = settings.token || '';
  $('showName').checked = settings.showName !== false;
  socket = new WebSocket('ws://127.0.0.1:' + port);
  socket.onopen = function () { socket.send(JSON.stringify({ event: registerEvent, uuid: uuid })); };
  ['url', 'token'].forEach(function (id) { $(id).addEventListener('change', function () { save(); loadDevices(); }); });
  $('device').addEventListener('change', save);
  $('showName').addEventListener('change', save);
  $('reload').addEventListener('click', loadDevices);
  loadDevices();
}
