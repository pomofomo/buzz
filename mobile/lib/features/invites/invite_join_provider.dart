import 'dart:convert';

import 'package:http/http.dart' as http;
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../shared/auth/auth.dart';
import '../../shared/deeplink/deep_link.dart';

final inviteJoinHttpClientProvider = Provider<http.Client>((ref) {
  final client = http.Client();
  ref.onDispose(client.close);
  return client;
});

enum InviteJoinStatus {
  idle,
  confirming,
  claiming,
  success,
  switchedExisting,
  error,
}

class InviteJoinState {
  final InviteJoinStatus status;
  final InviteDeepLink? invite;
  final String? host;
  final String? communityName;
  final String? errorMessage;
  final bool requiresFreshInvite;

  const InviteJoinState({
    this.status = InviteJoinStatus.idle,
    this.invite,
    this.host,
    this.communityName,
    this.errorMessage,
    this.requiresFreshInvite = false,
  });

  InviteJoinState copyWith({
    InviteJoinStatus? status,
    InviteDeepLink? invite,
    String? host,
    String? communityName,
    String? errorMessage,
    bool? requiresFreshInvite,
  }) => InviteJoinState(
    status: status ?? this.status,
    invite: invite ?? this.invite,
    host: host ?? this.host,
    communityName: communityName ?? this.communityName,
    errorMessage: errorMessage ?? this.errorMessage,
    requiresFreshInvite: requiresFreshInvite ?? this.requiresFreshInvite,
  );
}

class InviteJoinNotifier extends Notifier<InviteJoinState> {
  @override
  InviteJoinState build() => const InviteJoinState();

  Future<void> prepare(InviteDeepLink invite) async {
    final communities = await ref.read(communityListProvider.future);
    final existing = _existingCommunity(communities, invite.relayUrl);
    if (existing != null) {
      await ref
          .read(communityListProvider.notifier)
          .switchCommunity(existing.id);
      state = InviteJoinState(
        status: InviteJoinStatus.switchedExisting,
        invite: invite,
        host: _hostFromRelay(invite.relayUrl),
        communityName: existing.name,
      );
      return;
    }

    state = InviteJoinState(
      status: InviteJoinStatus.confirming,
      invite: invite,
      host: _hostFromRelay(invite.relayUrl),
      communityName: Community.nameFromUrl(invite.relayUrl),
    );
  }

  Future<void> confirmJoin() async {
    final invite = state.invite;
    if (invite == null ||
        state.requiresFreshInvite ||
        (state.status != InviteJoinStatus.confirming &&
            state.status != InviteJoinStatus.error)) {
      return;
    }

    state = state.copyWith(status: InviteJoinStatus.claiming);
    try {
      final communities = await ref.read(communityListProvider.future);
      final existing = _existingCommunity(communities, invite.relayUrl);
      if (existing != null) {
        await ref
            .read(communityListProvider.notifier)
            .switchCommunity(existing.id);
        state = state.copyWith(
          status: InviteJoinStatus.switchedExisting,
          communityName: existing.name,
        );
        return;
      }

      // The claim endpoint is unauthenticated — claiming an invite is how the
      // caller obtains an API key. The relay issues and returns the key (and
      // the actor id it minted) in the response body.
      final body = jsonEncode({
        'code': invite.code,
        if (invite.policyReceipt != null)
          'policy_receipt': invite.policyReceipt,
      });
      final url = _claimUrlFromRelay(invite.relayUrl);
      final response = await ref
          .read(inviteJoinHttpClientProvider)
          .post(
            Uri.parse(url),
            headers: {'Content-Type': 'application/json'},
            body: body,
          );
      final decoded = jsonDecode(response.body.isEmpty ? '{}' : response.body);
      if (response.statusCode < 200 || response.statusCode >= 300) {
        final message = decoded is Map && decoded['error'] is String
            ? decoded['error'] as String
            : 'HTTP ${response.statusCode}';
        throw InviteClaimException(message);
      }
      if (decoded is! Map) {
        throw const FormatException('Invite claim returned malformed JSON');
      }
      final claim = Map<String, dynamic>.from(decoded);

      final apiKey = _stringField(claim, const ['api_key', 'apiKey', 'token', 'key']);
      if (apiKey == null || apiKey.isEmpty) {
        throw const InviteClaimException(
          'Invite claim did not return an API key.',
        );
      }
      final actor = _stringField(claim, const ['pubkey', 'actor', 'actor_id']);

      final community = Community.create(
        name: _communityNameFromClaim(claim, invite.relayUrl),
        relayUrl: invite.relayUrl,
        pubkey: actor,
        apiKey: apiKey,
      );
      await ref
          .read(authProvider.notifier)
          .authenticateWithCommunity(community);
      state = state.copyWith(
        status: InviteJoinStatus.success,
        communityName: community.name,
      );
    } catch (error) {
      final requiresFreshInvite = _requiresFreshInvite(error);
      state = state.copyWith(
        status: InviteJoinStatus.error,
        errorMessage: _friendlyInviteError(error),
        requiresFreshInvite: requiresFreshInvite,
      );
    }
  }

  void reset() {
    state = const InviteJoinState();
  }
}

final inviteJoinProvider =
    NotifierProvider<InviteJoinNotifier, InviteJoinState>(
      InviteJoinNotifier.new,
    );

class InviteClaimException implements Exception {
  final String message;

  const InviteClaimException(this.message);

  @override
  String toString() => message;
}

Community? _existingCommunity(List<Community> communities, String relayUrl) {
  final invite = _relayOriginForComparison(relayUrl);
  for (final community in communities) {
    final current = _relayOriginForComparison(community.relayUrl);
    if (current == null) continue;
    if (current == invite) {
      return community;
    }
  }
  return null;
}

({bool secure, String host, int? port})? _relayOriginForComparison(String url) {
  final uri = Uri.tryParse(url);
  if (uri == null || uri.host.isEmpty) return null;
  final secure = switch (uri.scheme) {
    'https' || 'wss' => true,
    'http' || 'ws' => false,
    _ => null,
  };
  if (secure == null) return null;
  return (
    secure: secure,
    host: uri.host.toLowerCase(),
    port: _effectivePort(uri),
  );
}

int? _effectivePort(Uri uri) {
  if (uri.hasPort) return uri.port;
  return switch (uri.scheme) {
    'https' || 'wss' => 443,
    'http' || 'ws' => 80,
    _ => null,
  };
}

String _hostFromRelay(String relayUrl) {
  final uri = Uri.parse(relayUrl);
  if (uri.hasPort) return '${uri.host}:${uri.port}';
  return uri.host;
}

String _claimUrlFromRelay(String relayUrl) {
  final uri = Uri.parse(relayUrl);
  final scheme = switch (uri.scheme) {
    'wss' => 'https',
    'ws' => 'http',
    _ => throw FormatException('Invalid relay URL scheme: ${uri.scheme}'),
  };
  return Uri(
    scheme: scheme,
    host: uri.host,
    port: uri.hasPort ? uri.port : null,
    path: '/api/invites/claim',
  ).toString();
}

String? _stringField(Map<String, dynamic> claim, List<String> keys) {
  for (final key in keys) {
    final value = claim[key];
    if (value is String && value.trim().isNotEmpty) return value.trim();
  }
  return null;
}

String _communityNameFromClaim(Map<String, dynamic> claim, String relayUrl) {
  final host = claim['host'];
  if (host is String && host.trim().isNotEmpty) return host.trim();
  return Community.nameFromUrl(relayUrl);
}

bool _requiresFreshInvite(Object error) {
  return error.toString().contains('join_policy_required');
}

String _friendlyInviteError(Object error) {
  final message = error.toString();
  if (message.contains('invite_expired')) return 'This invite has expired.';
  if (message.contains('invite_invalid')) return 'This invite is not valid.';
  if (message.contains('join_policy_required')) {
    return 'This invite approval has expired. Re-open the invite link to try again.';
  }
  if (message.contains('SocketException') ||
      message.contains('Connection refused') ||
      message.contains('Network is unreachable') ||
      message.contains('No route to host')) {
    return 'Could not reach the relay. Check your connection and try again.';
  }
  return 'Could not join this community: $message';
}
