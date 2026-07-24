import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:buzz/shared/auth/auth_provider.dart';
import 'package:buzz/shared/community/community.dart';
import 'package:buzz/shared/community/community_provider.dart';
import 'package:buzz/shared/community/community_storage.dart';

import '../community/community_storage_test.dart';

void main() {
  test(
    'removes a community without an API key instead of authenticating',
    () async {
      // A stored community that carries no bearer API key can no longer
      // authenticate under the API-key model — build() should drop it.
      final storage = CommunityStorage(secure: FakeSecureStorage());
      final invalid = Community.create(
        name: 'Invalid',
        relayUrl: 'https://relay.example',
      );
      await storage.save(invalid);
      await storage.saveActiveId(invalid.id);
      final container = ProviderContainer(
        overrides: [communityStorageProvider.overrideWithValue(storage)],
      );
      addTearDown(container.dispose);

      final auth = await container.read(authProvider.future);

      expect(auth.status, AuthStatus.unauthenticated);
      expect(await storage.loadAll(), isEmpty);
      expect(await storage.loadActiveId(), isNull);
    },
  );

  test('falls through to the next community that has an API key', () async {
    final storage = CommunityStorage(secure: FakeSecureStorage());
    final invalid = Community.create(
      name: 'Invalid',
      relayUrl: 'https://invalid.example',
    );
    final valid = Community.create(
      name: 'Valid',
      relayUrl: 'https://valid.example',
      pubkey: 'a' * 64,
      apiKey: 'buzzk_valid',
    );
    await storage.save(invalid);
    await storage.save(valid);
    await storage.saveActiveId(invalid.id);
    final container = ProviderContainer(
      overrides: [communityStorageProvider.overrideWithValue(storage)],
    );
    addTearDown(container.dispose);

    final auth = await container.read(authProvider.future);

    expect(auth.status, AuthStatus.authenticated);
    expect(auth.community?.id, valid.id);
    expect(auth.community?.apiKey, 'buzzk_valid');
    expect(await storage.loadActiveId(), valid.id);
  });

  test(
    'authenticateWithCommunity stores and activates the community',
    () async {
      final storage = CommunityStorage(secure: FakeSecureStorage());
      final container = ProviderContainer(
        overrides: [communityStorageProvider.overrideWithValue(storage)],
      );
      addTearDown(container.dispose);
      await container.read(authProvider.future);

      final community = Community.create(
        name: 'Joined',
        relayUrl: 'https://joined.example',
        pubkey: 'b' * 64,
        apiKey: 'buzzk_joined',
      );
      await container
          .read(authProvider.notifier)
          .authenticateWithCommunity(community);

      final auth = container.read(authProvider).value;
      expect(auth?.status, AuthStatus.authenticated);
      expect(auth?.community?.id, community.id);
      expect(await storage.loadActiveId(), community.id);
      expect((await storage.loadAll()).single.apiKey, 'buzzk_joined');
    },
  );
}
