import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart' as http_testing;
import 'package:buzz/shared/relay/relay.dart';

const _channelId = '11111111-1111-4111-8111-111111111111';
const _apiKey = 'buzzk_test_key';

void main() {
  group('queryRelay bearer auth', () {
    test('sends Authorization: Bearer over POST /query', () async {
      http.Request? capturedRequest;
      final client = http_testing.MockClient((request) async {
        capturedRequest = request;
        return http.Response('[]', 200);
      });
      final session = RelaySessionNotifier(httpClient: client);
      final container = ProviderContainer(
        overrides: [
          relaySessionProvider.overrideWith(() => session),
          relayConfigProvider.overrideWith(
            () => _FakeRelayConfigNotifier(
              baseUrl: 'https://relay.example/base',
              apiKey: _apiKey,
            ),
          ),
        ],
      );
      addTearDown(container.dispose);

      const filter = NostrFilter(
        kinds: EventKind.channelTimelineContentKinds,
        tags: {
          '#h': [_channelId],
        },
        limit: 50,
        extensions: {
          'top_level': true,
          'include_summaries': true,
          'include_aux': true,
        },
      );

      await container.read(relaySessionProvider.notifier).queryRelay([filter]);

      expect(capturedRequest, isNotNull);
      expect(capturedRequest!.method, 'POST');
      expect(capturedRequest!.url.toString(), 'https://relay.example/query');
      expect(capturedRequest!.headers['Content-Type'], 'application/json');
      expect(capturedRequest!.headers['Authorization'], 'Bearer $_apiKey');
      // The client no longer signs the payload — no NIP-98 `Nostr ` header.
      expect(
        capturedRequest!.headers['Authorization'],
        isNot(startsWith('Nostr ')),
      );
      expect(jsonDecode(capturedRequest!.body), [filter.toJson()]);
    });

    test('omits the Authorization header when no API key is set', () async {
      http.Request? capturedRequest;
      final client = http_testing.MockClient((request) async {
        capturedRequest = request;
        return http.Response('[]', 200);
      });
      final session = RelaySessionNotifier(httpClient: client);
      final container = ProviderContainer(
        overrides: [
          relaySessionProvider.overrideWith(() => session),
          relayConfigProvider.overrideWith(
            () => _FakeRelayConfigNotifier(
              baseUrl: 'https://relay.example',
              apiKey: null,
            ),
          ),
        ],
      );
      addTearDown(container.dispose);

      await container.read(relaySessionProvider.notifier).queryRelay(const [
        NostrFilter(kinds: [EventKind.streamMessage]),
      ]);

      expect(capturedRequest, isNotNull);
      expect(capturedRequest!.headers.containsKey('Authorization'), isFalse);
    });

    test('rejects malformed event arrays', () async {
      final session = RelaySessionNotifier(
        httpClient: http_testing.MockClient(
          (_) async => http.Response('[{}]', 200),
        ),
      );
      final container = ProviderContainer(
        overrides: [
          relaySessionProvider.overrideWith(() => session),
          relayConfigProvider.overrideWith(
            () => _FakeRelayConfigNotifier(
              baseUrl: 'https://relay.example',
              apiKey: _apiKey,
            ),
          ),
        ],
      );
      addTearDown(container.dispose);

      await expectLater(
        container.read(relaySessionProvider.notifier).queryRelay(const [
          NostrFilter(kinds: [EventKind.streamMessage]),
        ]),
        throwsA(isA<FormatException>()),
      );
    });
  });

  group('history + reconnection', () {
    test(
      'history timeout rejects instead of returning partial empty data',
      () async {
        final session = RelaySessionNotifier();

        await expectLater(
          session.fetchHistory(
            const NostrFilter(kinds: [EventKind.channelThreadSummary]),
            timeout: const Duration(milliseconds: 1),
          ),
          throwsA(isA<TimeoutException>()),
        );
      },
    );

    test('background disconnect rejects in-flight history', () async {
      final session = RelaySessionNotifier();
      final container = ProviderContainer(
        overrides: [relaySessionProvider.overrideWith(() => session)],
      );
      addTearDown(container.dispose);
      container.read(relaySessionProvider);

      final history = session.fetchHistory(
        const NostrFilter(kinds: [EventKind.channelThreadSummary]),
        timeout: const Duration(seconds: 1),
      );
      final expectation = expectLater(history, throwsException);

      session.debugPauseNow();

      await expectation;
    });

    test('retries a dropped connected session without live subscriptions', () {
      final session = RelaySessionNotifier();
      final container = ProviderContainer(
        overrides: [relaySessionProvider.overrideWith(() => session)],
      );
      addTearDown(container.dispose);
      container.read(relaySessionProvider);

      session.debugHandleConnected();
      session.debugHandleDisconnected();

      expect(session.state.status, SessionStatus.reconnecting);
      expect(session.state.reconnectAttempt, 1);
    });

    test('stops reconnecting after a hard bearer auth rejection', () {
      final session = RelaySessionNotifier();
      final container = ProviderContainer(
        overrides: [relaySessionProvider.overrideWith(() => session)],
      );
      addTearDown(container.dispose);
      container.read(relaySessionProvider);

      session.debugHandleConnected();
      session.debugHandleDisconnected(
        const RelayAuthRejectedException('invalid bearer token'),
      );

      expect(session.state.status, SessionStatus.disconnected);
    });

    test('does not schedule reconnects after background disconnect', () {
      final session = RelaySessionNotifier();
      final container = ProviderContainer(
        overrides: [relaySessionProvider.overrideWith(() => session)],
      );
      addTearDown(container.dispose);
      container.read(relaySessionProvider);

      session.debugHandleConnected();
      session.debugPauseNow();
      session.debugHandleDisconnected();

      expect(session.state.status, SessionStatus.disconnected);
    });
  });

  group('live subscriptions', () {
    test(
      'delivers the same live event to each matching subscription',
      () async {
        final session = RelaySessionNotifier();
        final firstEvents = <NostrEvent>[];
        final secondEvents = <NostrEvent>[];
        const filter = NostrFilter(
          kinds: EventKind.channelEventKinds,
          tags: {
            '#h': [_channelId],
          },
          limit: 50,
        );

        final firstSubscribe = session.subscribe(filter, firstEvents.add);
        session.debugHandleMessage(['EOSE', 'l-1']);
        final unsubscribeFirst = await firstSubscribe;

        final secondSubscribe = session.subscribe(filter, secondEvents.add);
        session.debugHandleMessage(['EOSE', 'l-2']);
        final unsubscribeSecond = await secondSubscribe;

        final event = _event();
        session.debugHandleMessage(['EVENT', 'l-1', event.toJson()]);
        session.debugHandleMessage(['EVENT', 'l-2', event.toJson()]);
        session.debugFlushEventBuffer();

        expect(firstEvents.map((event) => event.id), [event.id]);
        expect(secondEvents.map((event) => event.id), [event.id]);

        // A duplicate delivery on the same subscription is de-duped.
        session.debugHandleMessage(['EVENT', 'l-1', event.toJson()]);
        session.debugFlushEventBuffer();

        expect(firstEvents.map((event) => event.id), [event.id]);
        expect(secondEvents.map((event) => event.id), [event.id]);

        unsubscribeFirst();
        unsubscribeSecond();
      },
    );

    test('live subscribe fails when relay closes before ready', () async {
      final session = RelaySessionNotifier();
      const filter = NostrFilter(
        kinds: [EventKind.agentObserverFrame],
        limit: 0,
      );

      final subscribe = session.subscribe(filter, (_) {});
      session.debugHandleMessage([
        'CLOSED',
        'l-1',
        'restricted: p-gated events require #p matching your pubkey',
      ]);

      await expectLater(
        subscribe,
        throwsA(
          isA<Exception>().having(
            (error) => error.toString(),
            'message',
            contains('p-gated events require #p'),
          ),
        ),
      );
    });

    test(
      'live onClosed callback runs when relay closes an open subscription',
      () async {
        final session = RelaySessionNotifier();
        final closedMessages = <String>[];
        const filter = NostrFilter(
          kinds: [EventKind.agentObserverFrame],
          limit: 0,
        );

        final subscribe = session.subscribe(
          filter,
          (_) {},
          onClosed: closedMessages.add,
        );
        session.debugHandleMessage(['EOSE', 'l-1']);
        final unsubscribe = await subscribe;
        session.debugHandleMessage([
          'CLOSED',
          'l-1',
          'restricted: no longer valid',
        ]);

        expect(closedMessages, ['restricted: no longer valid']);
        unsubscribe();
      },
    );
  });

  group('OK-by-id correlation (server-authored ingest)', () {
    test('publish resolves the pending intent when the OK id matches', () async {
      final session = RelaySessionNotifier();
      final container = ProviderContainer(
        overrides: [relaySessionProvider.overrideWith(() => session)],
      );
      addTearDown(container.dispose);
      container.read(relaySessionProvider);

      // Build the unsigned intent exactly as SignedEventRelay.submit does. The
      // client computes the NIP-01 content-hash id so the relay's OK frame —
      // keyed on the server-derived id — correlates back to this publish.
      final intent = buildUnsignedEvent(
        pubkey: 'abc123',
        kind: EventKind.streamMessage,
        content: 'hello',
        tags: const [
          ['h', 'chan-1'],
        ],
        createdAt: 1700000000,
      );
      expect(intent.sig, isEmpty);

      final pending = session.publish(intent);
      session.debugHandleMessage(['OK', intent.id, true, 'response:{}']);

      final ok = await pending;
      expect(ok.id, intent.id);
      expect(ok.content, 'response:{}');
    });

    test('publish rejects when the relay returns OK false', () async {
      final session = RelaySessionNotifier();
      final container = ProviderContainer(
        overrides: [relaySessionProvider.overrideWith(() => session)],
      );
      addTearDown(container.dispose);
      container.read(relaySessionProvider);

      final intent = buildUnsignedEvent(
        pubkey: 'abc123',
        kind: EventKind.streamMessage,
        content: 'nope',
        tags: const [],
        createdAt: 1700000000,
      );

      final pending = session.publish(intent);
      session.debugHandleMessage([
        'OK',
        intent.id,
        false,
        'restricted: missing messages:write',
      ]);

      await expectLater(
        pending,
        throwsA(
          isA<Exception>().having(
            (error) => error.toString(),
            'message',
            contains('missing messages:write'),
          ),
        ),
      );
    });
  });

  group('NIP-01 id computation matches the server serialization', () {
    test('matches a known SHA-256 vector for [0,pk,ts,kind,tags,content]', () {
      // Independent NIP-01 vector: sha256 of the compact JSON
      // [0,"",0,1,[],"hi"] (verified out-of-band). Confirms buildUnsignedEvent
      // uses the same serialization the relay's ingest re-derives.
      final event = buildUnsignedEvent(
        pubkey: '',
        kind: EventKind.note,
        content: 'hi',
        tags: const [],
        createdAt: 0,
      );
      expect(
        event.id,
        '504252a0008eac659c10c9be5918316bf998755c257875c1dc7066427bdec668',
      );
    });

    test('matches a known vector with a populated pubkey/tags/content', () {
      final event = buildUnsignedEvent(
        pubkey: 'abc123',
        kind: EventKind.streamMessage,
        content: 'hello',
        tags: const [
          ['h', 'chan-1'],
        ],
        createdAt: 1700000000,
      );
      expect(
        event.id,
        '88761496dbb213c727cfd58d28a4d4cc1fab202c9d669a2262af1669f3a1e151',
      );
      expect(event.id.length, 64);
    });
  });
}

class _FakeRelayConfigNotifier extends RelayConfigNotifier {
  final String _baseUrl;
  final String? _apiKey;

  _FakeRelayConfigNotifier({required String baseUrl, required String? apiKey})
    : _baseUrl = baseUrl,
      _apiKey = apiKey;

  @override
  RelayConfig build() => RelayConfig(baseUrl: _baseUrl, apiKey: _apiKey);
}

NostrEvent _event() {
  return const NostrEvent(
    id: 'event-1',
    pubkey: 'alice',
    createdAt: 20,
    kind: EventKind.streamMessageV2,
    tags: [
      ['h', _channelId],
    ],
    content: 'hello',
    sig: '',
  );
}
