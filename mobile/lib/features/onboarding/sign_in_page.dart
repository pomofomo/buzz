import 'package:flutter/material.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../shared/auth/auth.dart';
import '../../shared/theme/theme.dart';

/// API-key sign-in screen. Replaces the deleted NIP-AB pairing flow: a
/// community is provisioned by entering its relay URL and an API key issued by
/// the community admin (optionally the actor id that was issued alongside it).
class SignInPage extends HookConsumerWidget {
  /// When true this screen is being used to add another community and should
  /// pop itself on success rather than relying on the auth-state root swap.
  final bool addingCommunity;

  const SignInPage({super.key, this.addingCommunity = false});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final relayController = useTextEditingController();
    final keyController = useTextEditingController();
    final actorController = useTextEditingController();
    final submitting = useState(false);
    final errorMessage = useState<String?>(null);

    Future<void> connect() async {
      final relayUrl = _normalizeRelayUrl(relayController.text.trim());
      final apiKey = keyController.text.trim();
      final actor = actorController.text.trim();

      if (relayUrl == null) {
        errorMessage.value = 'Enter a valid relay URL (http:// or https://).';
        return;
      }
      if (apiKey.isEmpty) {
        errorMessage.value = 'Enter the API key issued for this community.';
        return;
      }

      submitting.value = true;
      errorMessage.value = null;
      try {
        final community = Community.create(
          name: Community.nameFromUrl(relayUrl),
          relayUrl: relayUrl,
          apiKey: apiKey,
          pubkey: actor.isEmpty ? null : actor,
        );
        await ref
            .read(authProvider.notifier)
            .authenticateWithCommunity(community);
        if (addingCommunity && context.mounted) {
          Navigator.of(context).pop();
        }
      } catch (error) {
        errorMessage.value = 'Could not connect: $error';
      } finally {
        submitting.value = false;
      }
    }

    return Scaffold(
      appBar: addingCommunity ? AppBar(title: const Text('Add Community')) : null,
      body: SafeArea(
        child: Center(
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(Grid.sm),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Icon(
                  LucideIcons.messageSquare,
                  size: 48,
                  color: context.colors.primary,
                ),
                const SizedBox(height: Grid.sm),
                Text(
                  'Connect to Buzz',
                  textAlign: TextAlign.center,
                  style: context.textTheme.titleLarge,
                ),
                const SizedBox(height: Grid.xxs),
                Text(
                  'Enter the relay URL and the API key your community admin '
                  'issued you.',
                  textAlign: TextAlign.center,
                  style: context.textTheme.bodyMedium?.copyWith(
                    color: context.colors.onSurfaceVariant,
                  ),
                ),
                const SizedBox(height: Grid.lg),
                TextField(
                  controller: relayController,
                  autocorrect: false,
                  keyboardType: TextInputType.url,
                  enabled: !submitting.value,
                  decoration: const InputDecoration(
                    labelText: 'Relay URL',
                    hintText: 'https://relay.example.com',
                    prefixIcon: Icon(LucideIcons.server),
                  ),
                ),
                const SizedBox(height: Grid.sm),
                TextField(
                  controller: keyController,
                  autocorrect: false,
                  obscureText: true,
                  enabled: !submitting.value,
                  decoration: const InputDecoration(
                    labelText: 'API key',
                    prefixIcon: Icon(LucideIcons.key),
                  ),
                ),
                const SizedBox(height: Grid.sm),
                TextField(
                  controller: actorController,
                  autocorrect: false,
                  enabled: !submitting.value,
                  decoration: const InputDecoration(
                    labelText: 'Actor id (optional)',
                    prefixIcon: Icon(LucideIcons.user),
                  ),
                ),
                if (errorMessage.value != null) ...[
                  const SizedBox(height: Grid.sm),
                  Text(
                    errorMessage.value!,
                    style: context.textTheme.bodySmall?.copyWith(
                      color: context.colors.error,
                    ),
                  ),
                ],
                const SizedBox(height: Grid.lg),
                FilledButton.icon(
                  onPressed: submitting.value ? null : connect,
                  icon: submitting.value
                      ? const SizedBox(
                          width: 16,
                          height: 16,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Icon(LucideIcons.arrowRight),
                  label: Text(submitting.value ? 'Connecting…' : 'Connect'),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Normalize a user-entered relay URL to an `http`/`https` base URL, converting
/// `ws`/`wss` schemes. Returns null when the input is not a usable URL.
String? _normalizeRelayUrl(String input) {
  if (input.isEmpty) return null;
  var value = input;
  if (value.startsWith('ws://')) {
    value = 'http://${value.substring('ws://'.length)}';
  } else if (value.startsWith('wss://')) {
    value = 'https://${value.substring('wss://'.length)}';
  } else if (!value.startsWith('http://') && !value.startsWith('https://')) {
    value = 'https://$value';
  }
  final uri = Uri.tryParse(value);
  if (uri == null || uri.host.isEmpty) return null;
  return value;
}
