import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:buzz/features/channels/agent_activity/observer_models.dart';
import 'package:buzz/features/channels/agent_activity/observer_subscription.dart';
import 'package:buzz/shared/relay/relay.dart';

// Opaque actor id (lowercased hex) for the signed-in bearer principal, and an
// agent id being observed.
const _actor =
    'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const _agent =
    'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';

void main() {
  test('reads the provider without a circular-dependency error', () {
    final container = ProviderContainer(
      overrides: [
        relaySessionProvider.overrideWith(() => _RecordingRelaySession()),
        relayConfigProvider.overrideWith(
          () => _FakeRelayConfigNotifier(actorPubkey: null),
        ),
      ],
    );
    addTearDown(container.dispose);

    const key = (channelId: 'test-channel', agentPubkey: _agent);
    final state = container.read(observerSubscriptionProvider(key));
    expect(state.connection, ObserverConnectionState.idle);
    expect(state.transcript, isEmpty);
  });

  test('stays idle when no actor id is provisioned', () async {
    final relaySession = _RecordingRelaySession();
    final container = ProviderContainer(
      overrides: [
        relaySessionProvider.overrideWith(() => relaySession),
        relayConfigProvider.overrideWith(
          () => _FakeRelayConfigNotifier(actorPubkey: null),
        ),
      ],
    );
    addTearDown(container.dispose);

    const key = (channelId: 'test-channel', agentPubkey: _agent);
    container.read(observerSubscriptionProvider(key));
    await Future<void>.delayed(Duration.zero);

    final state = container.read(observerSubscriptionProvider(key));
    expect(state.connection, ObserverConnectionState.idle);
    expect(relaySession.filters, isEmpty);
  });

  test(
    'subscribes with the correct #p filter shape and transitions to open',
    () async {
      final relaySession = _RecordingRelaySession();
      final container = ProviderContainer(
        overrides: [
          relaySessionProvider.overrideWith(() => relaySession),
          relayConfigProvider.overrideWith(
            () => _FakeRelayConfigNotifier(actorPubkey: _actor),
          ),
        ],
      );
      addTearDown(container.dispose);

      const key = (channelId: 'test-channel', agentPubkey: _agent);
      container.read(observerSubscriptionProvider(key));
      await Future<void>.delayed(Duration.zero);

      final state = container.read(observerSubscriptionProvider(key));
      expect(state.connection, ObserverConnectionState.open);
      expect(state.transcript, isEmpty);

      expect(relaySession.filters, hasLength(1));
      final filter = relaySession.filters.first;
      expect(filter.kinds, [EventKind.agentObserverFrame]);
      expect(filter.limit, 0);
      expect(filter.tags['#p'], contains(_actor));
      expect(filter.since, isNull);
    },
  );

  test(
    'uses one shared relay subscription for channel-scoped readers',
    () async {
      final relaySession = _RecordingRelaySession();
      final container = ProviderContainer(
        overrides: [
          relaySessionProvider.overrideWith(() => relaySession),
          relayConfigProvider.overrideWith(
            () => _FakeRelayConfigNotifier(actorPubkey: _actor),
          ),
        ],
      );
      addTearDown(container.dispose);

      container.read(
        observerSubscriptionProvider((
          channelId: 'first-channel',
          agentPubkey: _agent,
        )),
      );
      container.read(
        observerSubscriptionProvider((
          channelId: 'second-channel',
          agentPubkey: _agent,
        )),
      );
      await Future<void>.delayed(Duration.zero);

      expect(relaySession.filters, hasLength(1));
    },
  );

  test('surfaces relay CLOSED messages through observer state', () async {
    final relaySession = _RecordingRelaySession();
    final container = ProviderContainer(
      overrides: [
        relaySessionProvider.overrideWith(() => relaySession),
        relayConfigProvider.overrideWith(
          () => _FakeRelayConfigNotifier(actorPubkey: _actor),
        ),
      ],
    );
    addTearDown(container.dispose);

    const key = (channelId: 'test-channel', agentPubkey: _agent);
    container.read(observerSubscriptionProvider(key));
    await Future<void>.delayed(Duration.zero);

    relaySession.closeAll('restricted: p-gated events require #p');

    final state = container.read(observerSubscriptionProvider(key));
    expect(state.connection, ObserverConnectionState.error);
    expect(state.errorMessage, contains('p-gated events require #p'));
  });
}

class _RecordingRelaySession extends RelaySessionNotifier {
  final List<NostrFilter> filters = [];
  final List<void Function(String message)> _closedListeners = [];

  @override
  SessionState build() => const SessionState(status: SessionStatus.connected);

  @override
  Future<void Function()> subscribe(
    NostrFilter filter,
    void Function(NostrEvent) onEvent, {
    void Function(String message)? onClosed,
  }) async {
    filters.add(filter);
    if (onClosed != null) {
      _closedListeners.add(onClosed);
    }
    return () {
      filters.remove(filter);
      if (onClosed != null) {
        _closedListeners.remove(onClosed);
      }
    };
  }

  void closeAll(String message) {
    for (final listener in List.of(_closedListeners)) {
      listener(message);
    }
    filters.clear();
    _closedListeners.clear();
  }
}

class _FakeRelayConfigNotifier extends RelayConfigNotifier {
  final String? _actorPubkey;

  _FakeRelayConfigNotifier({required String? actorPubkey})
    : _actorPubkey = actorPubkey;

  @override
  RelayConfig build() => RelayConfig(
    baseUrl: 'http://localhost:3000',
    apiKey: 'buzzk_test',
    actorPubkey: _actorPubkey,
  );
}
