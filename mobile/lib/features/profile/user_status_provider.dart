import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../shared/relay/relay.dart';
import 'user_status.dart';
import 'user_status_cache_provider.dart';

/// Manages the current user's own NIP-38 status (kind:30315, d=general).
///
/// Fetches the existing status on build and provides [setStatus] / [clearStatus]
/// for publishing. Publishes via WebSocket (triggers fan-out). No heartbeat
/// needed — user status events are parameterised replaceable, not ephemeral.
class UserStatusNotifier extends AsyncNotifier<UserStatus?> {
  @override
  Future<UserStatus?> build() {
    ref.watch(relayClientProvider);
    ref.watch(relaySessionProvider);
    return _fetch();
  }

  Future<UserStatus?> _fetch() async {
    final config = ref.read(relayConfigProvider);
    final actor = config.actorPubkey?.trim();
    if (actor == null || actor.isEmpty) return null;
    final pubkey = actor.toLowerCase();

    final sessionState = ref.read(relaySessionProvider);
    if (sessionState.status != SessionStatus.connected) return null;

    try {
      final session = ref.read(relaySessionProvider.notifier);
      final events = await session.fetchHistory(
        NostrFilter(
          kinds: const [EventKind.userStatus],
          authors: [pubkey],
          tags: const {
            '#d': ['general'],
          },
          limit: 1,
        ),
      );

      if (events.isEmpty) return null;

      // Pick the most recent event.
      final latest = events.reduce(
        (a, b) => a.createdAt >= b.createdAt ? a : b,
      );
      final status = UserStatus.fromEvent(latest);
      return status.isEmpty ? null : status;
    } catch (_) {
      return null;
    }
  }

  Future<void> setStatus(String text, String emoji) async {
    final trimmed = text.trim();
    final config = ref.read(relayConfigProvider);
    final actor = config.actorPubkey?.trim();
    if (actor == null || actor.isEmpty) return;

    final tags = <List<String>>[
      ['d', 'general'],
    ];
    if (emoji.isNotEmpty) {
      tags.add(['emoji', emoji]);
    }

    final relay = SignedEventRelay(
      session: ref.read(relaySessionProvider.notifier),
      actorPubkey: actor,
    );
    await relay.submit(
      kind: EventKind.userStatus,
      content: trimmed,
      tags: tags,
    );

    // Optimistic update: update own state immediately.
    final newStatus = (trimmed.isNotEmpty || emoji.isNotEmpty)
        ? UserStatus(
            text: trimmed,
            emoji: emoji,
            updatedAt: DateTime.now().millisecondsSinceEpoch ~/ 1000,
          )
        : null;
    state = AsyncValue.data(newStatus);

    // Also update the shared cache so other UI reads stay consistent.
    final pubkey = actor.toLowerCase();
    ref.read(userStatusCacheProvider.notifier).updateStatus(pubkey, newStatus);
  }

  Future<void> clearStatus() => setStatus('', '');
}

final userStatusProvider =
    AsyncNotifierProvider<UserStatusNotifier, UserStatus?>(
      UserStatusNotifier.new,
    );
