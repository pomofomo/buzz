import 'dart:convert';
import 'dart:typed_data';

import 'package:pointycastle/digests/sha256.dart';

import 'nostr_models.dart';
import 'relay_session.dart';

/// Submits unsigned event *intents* through the relay WebSocket connection.
///
/// Under the API-key model the client no longer signs events: the bearer token
/// on the WebSocket connection authenticates the actor, and the relay authors
/// the persisted row. This helper builds the `{id, pubkey, kind, tags, content,
/// created_at}` envelope (with an empty `sig`) and publishes it. The `id` is the
/// NIP-01 content hash so it matches the id the relay re-derives server-side,
/// keeping the OK-by-id correlation intact.
///
/// The class name is retained for call-site stability across features.
class SignedEventRelay {
  final RelaySessionNotifier _session;
  final String? _actorPubkey;

  SignedEventRelay({
    required RelaySessionNotifier session,
    String? actorPubkey,
  }) : _session = session,
       _actorPubkey = actorPubkey;

  /// This actor's opaque id, or null when unknown (server still authors rows).
  String? get pubkey {
    final actor = _actorPubkey;
    if (actor == null || actor.isEmpty) return null;
    return actor;
  }

  /// Build an unsigned intent and submit it. Returns the relay's OK response as
  /// a [NostrEvent] whose `content` field contains the OK message (e.g.
  /// `"response:{...}"` for command kinds).
  Future<NostrEvent> submit({
    required int kind,
    required String content,
    required List<List<String>> tags,
    int? createdAt,
  }) async {
    final event = buildUnsignedEvent(
      pubkey: _actorPubkey ?? '',
      kind: kind,
      content: content,
      tags: tags,
      createdAt: createdAt,
    );
    return _session.publish(event);
  }
}

/// Build an unsigned NIP-01 event envelope with a content-hash `id` and an
/// empty `sig`. The server re-derives the same id and authors the row.
NostrEvent buildUnsignedEvent({
  required String pubkey,
  required int kind,
  required String content,
  required List<List<String>> tags,
  int? createdAt,
}) {
  final ts = createdAt ?? DateTime.now().millisecondsSinceEpoch ~/ 1000;
  final serialized = jsonEncode([0, pubkey, ts, kind, tags, content]);
  final digest = SHA256Digest().process(
    Uint8List.fromList(utf8.encode(serialized)),
  );
  final id = digest
      .map((byte) => byte.toRadixString(16).padLeft(2, '0'))
      .join();
  return NostrEvent(
    id: id,
    pubkey: pubkey,
    createdAt: ts,
    kind: kind,
    tags: tags,
    content: content,
    sig: '',
  );
}
