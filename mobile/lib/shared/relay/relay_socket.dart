import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:web_socket_channel/io.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

/// Low-level websocket connection authenticated with a bearer API key.
///
/// The API key is attached as an `Authorization: Bearer <key>` header on the
/// WebSocket upgrade request. There is no in-band challenge/response handshake:
/// the connection is considered ready as soon as the socket opens (the relay
/// authenticates the bearer at the upgrade and closes the socket if it is
/// invalid).
///
/// Does NOT handle reconnection — that is [RelaySessionNotifier]'s job.
enum SocketState { disconnected, connecting, connected }

/// Retained so [RelaySessionNotifier] can special-case a hard auth rejection.
/// With bearer auth an invalid key manifests as an upgrade/connection failure
/// rather than an in-band frame.
class RelayAuthRejectedException implements Exception {
  final String message;

  const RelayAuthRejectedException(this.message);

  @override
  String toString() => 'Relay authentication rejected: $message';
}

class RelaySocket {
  final String _wsUrl;
  final String? _apiKey;
  final void Function(List<dynamic> message) _onMessage;
  final void Function() _onConnected;
  final void Function(Object? error) _onDisconnected;

  WebSocketChannel? _channel;
  StreamSubscription<dynamic>? _subscription;
  SocketState _state = SocketState.disconnected;

  SocketState get state => _state;

  RelaySocket({
    required String wsUrl,
    required String? apiKey,
    required void Function(List<dynamic> message) onMessage,
    required void Function() onConnected,
    required void Function(Object? error) onDisconnected,
  }) : _wsUrl = wsUrl,
       _apiKey = apiKey,
       _onMessage = onMessage,
       _onConnected = onConnected,
       _onDisconnected = onDisconnected;

  /// Connect to the relay with the bearer API key attached.
  Future<void> connect() async {
    if (_state != SocketState.disconnected) return;
    _state = SocketState.connecting;

    final apiKey = _apiKey;
    final headers = <String, dynamic>{
      if (apiKey != null && apiKey.isNotEmpty) 'Authorization': 'Bearer $apiKey',
    };

    try {
      final channel = IOWebSocketChannel.connect(
        Uri.parse(_wsUrl),
        headers: headers,
      );
      _channel = channel;
      await channel.ready;
    } catch (e) {
      _state = SocketState.disconnected;
      _resetConnection();
      _onDisconnected(e);
      return;
    }

    // The channel may have been disposed while we were awaiting ready
    // (e.g. provider rebuild triggered dispose() concurrently).
    if (_channel == null) {
      _state = SocketState.disconnected;
      return;
    }

    _subscription = _channel!.stream.listen(
      _handleRawMessage,
      onError: (Object error) {
        _resetConnection();
        _onDisconnected(error);
      },
      onDone: () {
        _resetConnection();
        _onDisconnected(null);
      },
    );

    _state = SocketState.connected;
    _onConnected();
  }

  /// Send a raw JSON array over the websocket.
  void send(List<dynamic> payload) {
    _channel?.sink.add(jsonEncode(payload));
  }

  /// Gracefully close the connection.
  Future<void> disconnect() async {
    _resetConnection();
    final channel = _channel;
    _channel = null;
    if (channel != null) {
      await channel.sink.close();
    }
  }

  void dispose() {
    _resetConnection();
    _channel?.sink.close();
    _channel = null;
  }

  void _resetConnection() {
    _state = SocketState.disconnected;
    _subscription?.cancel();
    _subscription = null;
  }

  @visibleForTesting
  void debugHandleOkForTest(List<dynamic> data) => _onMessage(data);

  void _handleRawMessage(dynamic raw) {
    final String text;
    if (raw is String) {
      text = raw;
    } else {
      return; // Binary frames are not part of the protocol.
    }

    final List<dynamic> data;
    try {
      data = jsonDecode(text) as List<dynamic>;
    } catch (_) {
      return; // Malformed JSON.
    }

    if (data.isEmpty) return;
    // Forward every frame upstream (EVENT, EOSE, OK, CLOSED, NOTICE, …).
    _onMessage(data);
  }
}
