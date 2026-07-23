import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../community/community_provider.dart';
import 'relay_client.dart';

/// Relay connection configuration.
///
/// In the API-key world the client authenticates with a bearer token and
/// identifies itself by an opaque actor id (the value that used to live in the
/// Nostr `pubkey` column). No private signing key is held on the device.
///   - `baseUrl`      — where the relay lives (used for WS + media/HTTP)
///   - `apiKey`       — bearer token attached to WS upgrade + HTTP requests
///   - `actorPubkey`  — this actor's opaque id, for self-scoped filters/tags
class RelayConfig {
  final String baseUrl;

  /// Bearer API key for authentication.
  final String? apiKey;

  /// This actor's opaque id (hex), provisioned alongside the key.
  final String? actorPubkey;

  const RelayConfig({required this.baseUrl, this.apiKey, this.actorPubkey});

  /// Derive the websocket URL from the HTTP base URL.
  String get wsUrl {
    final uri = Uri.parse(baseUrl);
    final scheme = uri.scheme == 'https' ? 'wss' : 'ws';
    return uri.replace(scheme: scheme).toString();
  }
}

/// Compile-time environment config via --dart-define.
///
/// Run with:
///   flutter run --dart-define=BUZZ_RELAY_URL=http://localhost:3000
///
/// Or create a `.env.json` and use --dart-define-from-file=.env.json
class Env {
  static const relayUrl = String.fromEnvironment(
    'BUZZ_RELAY_URL',
    defaultValue: 'http://localhost:3000',
  );
}

class RelayConfigNotifier extends Notifier<RelayConfig> {
  @override
  RelayConfig build() {
    // Watch the active community so that when it changes (community switch),
    // the config rebuilds, triggering the full provider cascade.
    final activeAsync = ref.watch(activeCommunityProvider);
    final active = activeAsync.value;
    if (active != null) {
      return RelayConfig(
        baseUrl: active.relayUrl,
        apiKey: active.apiKey,
        actorPubkey: active.pubkey,
      );
    }

    // Fallback to compile-time env config (dev mode).
    return const RelayConfig(baseUrl: Env.relayUrl);
  }

  void update({required String baseUrl, String? apiKey, String? actorPubkey}) {
    state = RelayConfig(
      baseUrl: baseUrl,
      apiKey: apiKey,
      actorPubkey: actorPubkey,
    );
  }
}

final relayConfigProvider = NotifierProvider<RelayConfigNotifier, RelayConfig>(
  RelayConfigNotifier.new,
);

/// The current user's opaque actor id, from the active community.
final myPubkeyProvider = Provider<String?>((ref) {
  final config = ref.watch(relayConfigProvider);
  final actor = config.actorPubkey?.trim();
  if (actor == null || actor.isEmpty) return null;
  return actor;
});

/// Provides a [RelayClient] that reacts to config changes.
///
/// Only used for the media upload HTTP endpoint now — all data flow goes
/// through the relay WebSocket session.
final relayClientProvider = Provider<RelayClient>((ref) {
  final config = ref.watch(relayConfigProvider);
  final client = RelayClient(baseUrl: config.baseUrl);
  ref.onDispose(client.dispose);
  return client;
});
