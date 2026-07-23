import 'package:uuid/uuid.dart';

const _uuid = Uuid();
const _sentinel = Object();

class Community {
  final String id;
  final String name;
  final String relayUrl;

  /// Opaque 32-byte actor id (hex), provisioned alongside the API key. Occupies
  /// the same column shape the Nostr pubkey used to. Used for self-scoped
  /// filters/tags; the server is the authority on the authenticated actor.
  final String? pubkey;

  /// Bearer API key used to authenticate WebSocket + HTTP requests. Replaces
  /// the former per-device Nostr secret key — clients no longer hold a private
  /// signing key.
  final String? apiKey;
  final DateTime addedAt;

  const Community({
    required this.id,
    required this.name,
    required this.relayUrl,
    this.pubkey,
    this.apiKey,
    required this.addedAt,
  });

  factory Community.create({
    required String name,
    required String relayUrl,
    String? pubkey,
    String? apiKey,
  }) {
    return Community(
      id: _uuid.v4(),
      name: name,
      relayUrl: relayUrl,
      pubkey: pubkey,
      apiKey: apiKey,
      addedAt: DateTime.now(),
    );
  }

  Community copyWith({
    String? name,
    String? relayUrl,
    Object? pubkey = _sentinel,
    Object? apiKey = _sentinel,
  }) {
    return Community(
      id: id,
      name: name ?? this.name,
      relayUrl: relayUrl ?? this.relayUrl,
      pubkey: pubkey == _sentinel ? this.pubkey : pubkey as String?,
      apiKey: apiKey == _sentinel ? this.apiKey : apiKey as String?,
      addedAt: addedAt,
    );
  }

  Map<String, dynamic> toJson() => {
    'id': id,
    'name': name,
    'relayUrl': relayUrl,
    if (pubkey != null) 'pubkey': pubkey,
    if (apiKey != null) 'apiKey': apiKey,
    'addedAt': addedAt.toIso8601String(),
  };

  factory Community.fromJson(Map<String, dynamic> json) => Community(
    id: json['id'] as String,
    name: json['name'] as String,
    relayUrl: json['relayUrl'] as String,
    pubkey: json['pubkey'] as String?,
    apiKey: json['apiKey'] as String?,
    addedAt: DateTime.parse(json['addedAt'] as String),
  );

  /// Derive a human-friendly community name from a relay URL.
  static String nameFromUrl(String url) {
    try {
      final host = Uri.parse(url).host;
      if (host.contains('localhost') || host == '127.0.0.1') return 'Local Dev';
      final parts = host.split('.');
      if (parts.length > 2) return parts.first;
      return host;
    } catch (_) {
      return 'Community';
    }
  }
}
