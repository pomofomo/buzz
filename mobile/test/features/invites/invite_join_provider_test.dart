import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart' as http_testing;

import 'package:buzz/features/invites/invite_join_provider.dart';
import 'package:buzz/shared/auth/auth.dart';
import 'package:buzz/shared/deeplink/deep_link.dart';

import '../../shared/community/community_storage_test.dart';

// A plausible relay-issued key + actor, matching the apikey-mode claim contract:
// {status: "joined", community_id, host, role, api_key: "buzzk_<64hex>",
//  actor: "<64hex>"}.
const _issuedActor =
    '1111111111111111111111111111111111111111111111111111111111111111';
const _issuedApiKey =
    'buzzk_2222222222222222222222222222222222222222222222222222222222222222';

void main() {
  for (final existingRelayUrl in [
    'wss://relay.example.com',
    'https://relay.example.com',
  ]) {
    test(
      'same-relay invite switches existing $existingRelayUrl before any claim',
      () async {
        var claimRequests = 0;
        final storage = CommunityStorage(secure: FakeSecureStorage());
        final existing = Community(
          id: 'existing-id',
          name: 'Existing',
          relayUrl: existingRelayUrl,
          pubkey: 'old-actor',
          apiKey: 'buzzk_old',
          addedAt: DateTime.utc(2026),
        );
        await storage.save(existing);
        final auth = _RecordingAuthNotifier();
        final container = ProviderContainer(
          overrides: [
            communityStorageProvider.overrideWithValue(storage),
            authProvider.overrideWith(() => auth),
            inviteJoinHttpClientProvider.overrideWithValue(
              http_testing.MockClient((request) async {
                claimRequests++;
                return http.Response('{}', 500);
              }),
            ),
          ],
        );
        addTearDown(container.dispose);
        await container.read(communityListProvider.future);

        await container
            .read(inviteJoinProvider.notifier)
            .prepare(
              const InviteDeepLink(
                relayUrl: 'wss://relay.example.com',
                code: 'code',
              ),
            );

        final state = container.read(inviteJoinProvider);
        final stored = (await storage.loadAll()).single;
        expect(state.status, InviteJoinStatus.switchedExisting);
        expect(await storage.loadActiveId(), existing.id);
        expect(stored.relayUrl, existingRelayUrl);
        expect(stored.pubkey, 'old-actor');
        expect(stored.apiKey, 'buzzk_old');
        expect(claimRequests, 0);
        expect(auth.authenticatedCommunities, isEmpty);
      },
    );
  }

  test('unauthenticated claim stores the relay-issued key + actor', () async {
    http.Request? capturedRequest;
    final storage = CommunityStorage(secure: FakeSecureStorage());
    final auth = _RecordingAuthNotifier();
    final container = ProviderContainer(
      overrides: [
        communityStorageProvider.overrideWithValue(storage),
        authProvider.overrideWith(() => auth),
        inviteJoinHttpClientProvider.overrideWithValue(
          http_testing.MockClient((request) async {
            capturedRequest = request;
            return http.Response(
              jsonEncode({
                'status': 'joined',
                'community_id': 'community-uuid',
                'host': 'relay.example.com',
                'role': 'member',
                'api_key': _issuedApiKey,
                'actor': _issuedActor,
              }),
              200,
            );
          }),
        ),
      ],
    );
    addTearDown(container.dispose);

    await container
        .read(inviteJoinProvider.notifier)
        .prepare(
          const InviteDeepLink(
            relayUrl: 'wss://relay.example.com',
            code: 'code',
          ),
        );
    expect(
      container.read(inviteJoinProvider).status,
      InviteJoinStatus.confirming,
    );

    await container.read(inviteJoinProvider.notifier).confirmJoin();

    final state = container.read(inviteJoinProvider);
    expect(state.status, InviteJoinStatus.success);
    expect(capturedRequest, isNotNull);
    expect(
      capturedRequest!.url.toString(),
      'https://relay.example.com/api/invites/claim',
    );
    // The HMAC invite code is the credential — the claim is unauthenticated
    // and simply posts the code (no bearer / NIP-98 header).
    expect(capturedRequest!.body, jsonEncode({'code': 'code'}));
    expect(capturedRequest!.headers.containsKey('Authorization'), isFalse);

    expect(auth.authenticatedCommunities, hasLength(1));
    final joined = auth.authenticatedCommunities.single;
    expect(joined.relayUrl, 'wss://relay.example.com');
    expect(joined.pubkey, _issuedActor);
    expect(joined.apiKey, _issuedApiKey);
  });

  test('reads api_key/actor fallback field names', () async {
    final storage = CommunityStorage(secure: FakeSecureStorage());
    final auth = _RecordingAuthNotifier();
    final container = ProviderContainer(
      overrides: [
        communityStorageProvider.overrideWithValue(storage),
        authProvider.overrideWith(() => auth),
        inviteJoinHttpClientProvider.overrideWithValue(
          http_testing.MockClient((request) async {
            return http.Response(
              jsonEncode({
                'status': 'joined',
                'host': 'relay.example.com',
                'role': 'member',
                // Fallback aliases the client also accepts.
                'token': _issuedApiKey,
                'pubkey': _issuedActor,
              }),
              200,
            );
          }),
        ),
      ],
    );
    addTearDown(container.dispose);

    await container
        .read(inviteJoinProvider.notifier)
        .prepare(
          const InviteDeepLink(
            relayUrl: 'wss://relay.example.com',
            code: 'code',
          ),
        );
    await container.read(inviteJoinProvider.notifier).confirmJoin();

    expect(container.read(inviteJoinProvider).status, InviteJoinStatus.success);
    final joined = auth.authenticatedCommunities.single;
    expect(joined.apiKey, _issuedApiKey);
    expect(joined.pubkey, _issuedActor);
  });

  test('claim without an API key surfaces an error', () async {
    final storage = CommunityStorage(secure: FakeSecureStorage());
    final auth = _RecordingAuthNotifier();
    final container = ProviderContainer(
      overrides: [
        communityStorageProvider.overrideWithValue(storage),
        authProvider.overrideWith(() => auth),
        inviteJoinHttpClientProvider.overrideWithValue(
          http_testing.MockClient((request) async {
            return http.Response(
              jsonEncode({
                'status': 'joined',
                'host': 'relay.example.com',
                'role': 'member',
              }),
              200,
            );
          }),
        ),
      ],
    );
    addTearDown(container.dispose);

    await container
        .read(inviteJoinProvider.notifier)
        .prepare(
          const InviteDeepLink(
            relayUrl: 'wss://relay.example.com',
            code: 'code',
          ),
        );
    await container.read(inviteJoinProvider.notifier).confirmJoin();

    expect(container.read(inviteJoinProvider).status, InviteJoinStatus.error);
    expect(auth.authenticatedCommunities, isEmpty);
  });

  test('join_policy_required requires a fresh link and cannot retry', () async {
    var attempts = 0;
    final storage = CommunityStorage(secure: FakeSecureStorage());
    final container = ProviderContainer(
      overrides: [
        communityStorageProvider.overrideWithValue(storage),
        inviteJoinHttpClientProvider.overrideWithValue(
          http_testing.MockClient((request) async {
            attempts++;
            return http.Response(
              jsonEncode({'error': 'join_policy_required'}),
              403,
            );
          }),
        ),
      ],
    );
    addTearDown(container.dispose);

    await container
        .read(inviteJoinProvider.notifier)
        .prepare(
          const InviteDeepLink(
            relayUrl: 'wss://relay.example.com',
            code: 'code',
            policyReceipt: 'expired.receipt',
          ),
        );
    await container.read(inviteJoinProvider.notifier).confirmJoin();

    final state = container.read(inviteJoinProvider);
    expect(state.status, InviteJoinStatus.error);
    expect(state.requiresFreshInvite, isTrue);
    expect(
      state.errorMessage,
      'This invite approval has expired. Re-open the invite link to try again.',
    );

    await container.read(inviteJoinProvider.notifier).confirmJoin();
    expect(attempts, 1);
  });

  test(
    'failed claim can be retried and preserves the policy receipt',
    () async {
      var attempts = 0;
      final bodies = <String>[];
      final storage = CommunityStorage(secure: FakeSecureStorage());
      final auth = _RecordingAuthNotifier();
      final container = ProviderContainer(
        overrides: [
          communityStorageProvider.overrideWithValue(storage),
          authProvider.overrideWith(() => auth),
          inviteJoinHttpClientProvider.overrideWithValue(
            http_testing.MockClient((request) async {
              attempts++;
              bodies.add(request.body);
              if (attempts == 1) {
                return http.Response(jsonEncode({'error': 'temporary'}), 503);
              }
              return http.Response(
                jsonEncode({
                  'status': 'joined',
                  'host': 'relay.example.com',
                  'role': 'member',
                  'api_key': _issuedApiKey,
                  'actor': _issuedActor,
                }),
                200,
              );
            }),
          ),
        ],
      );
      addTearDown(container.dispose);

      await container
          .read(inviteJoinProvider.notifier)
          .prepare(
            const InviteDeepLink(
              relayUrl: 'wss://relay.example.com',
              code: 'code',
              policyReceipt: 'receipt.value',
            ),
          );
      await container.read(inviteJoinProvider.notifier).confirmJoin();
      expect(container.read(inviteJoinProvider).status, InviteJoinStatus.error);

      await container.read(inviteJoinProvider.notifier).confirmJoin();

      expect(
        container.read(inviteJoinProvider).status,
        InviteJoinStatus.success,
      );
      expect(attempts, 2);
      expect(
        bodies,
        everyElement(
          jsonEncode({'code': 'code', 'policy_receipt': 'receipt.value'}),
        ),
      );
      expect(auth.authenticatedCommunities, hasLength(1));
    },
  );
}

class _RecordingAuthNotifier extends AuthNotifier {
  final List<Community> authenticatedCommunities = [];

  @override
  Future<AuthState> build() async =>
      const AuthState(status: AuthStatus.unauthenticated);

  @override
  Future<void> authenticateWithCommunity(Community community) async {
    authenticatedCommunities.add(community);
    state = AsyncData(
      AuthState(status: AuthStatus.authenticated, community: community),
    );
  }
}
