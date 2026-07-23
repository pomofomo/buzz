import 'package:flutter/widgets.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import 'relay_provider.dart';

/// Builds `Authorization: Bearer <apiKey>` headers for relay-host media URLs.
///
/// Returns an empty map for non-relay URLs or when no API key is available, so
/// callers can safely use this on arbitrary profile/custom-emoji URLs without
/// leaking Buzz credentials to third-party hosts.
///
/// The service is rebuilt (via [mediaGetAuthServiceProvider]) whenever the relay
/// config — base URL or API key — changes.
class MediaGetAuthService {
  final String _baseUrl;
  final String? _apiKey;

  const MediaGetAuthService({required String baseUrl, required String? apiKey})
    : _baseUrl = baseUrl,
      _apiKey = apiKey;

  Map<String, String> headersFor(String url) {
    final apiKey = _apiKey;
    if (apiKey == null || apiKey.isEmpty) return const {};
    final uri = Uri.tryParse(url);
    final relayUri = Uri.tryParse(_baseUrl);
    if (uri == null || relayUri == null) return const {};
    if (!_isRelayMediaUrl(uri, relayUri)) return const {};

    return {'Authorization': 'Bearer $apiKey'};
  }

  bool _isRelayMediaUrl(Uri uri, Uri relayUri) {
    if (uri.scheme != 'http' && uri.scheme != 'https') return false;
    if (uri.host.isEmpty || relayUri.host.isEmpty) return false;
    // Extract the URL's origin and path. Query strings are ignored for media
    // host/path detection, matching the fetch target shape used by descriptors.
    final base = '${uri.scheme}://${uri.authority}';
    final mediaAuthority = extractServerAuthority(base);
    final relayAuthority = extractServerAuthority(_baseUrl);
    if (mediaAuthority == null || relayAuthority == null) return false;
    if (mediaAuthority.toLowerCase() != relayAuthority.toLowerCase()) {
      return false;
    }
    return uri.path.startsWith('/media/');
  }

  @override
  bool operator ==(Object other) =>
      other is MediaGetAuthService &&
      other._baseUrl == _baseUrl &&
      other._apiKey == _apiKey;

  @override
  int get hashCode => Object.hash(_baseUrl, _apiKey);
}

final mediaGetAuthServiceProvider = Provider<MediaGetAuthService>((ref) {
  final config = ref.watch(relayConfigProvider);
  return MediaGetAuthService(baseUrl: config.baseUrl, apiKey: config.apiKey);
});

Map<String, String> mediaGetHeadersFor(WidgetRef ref, String url) {
  return ref.read(mediaGetAuthServiceProvider).headersFor(url);
}

Map<String, String> mediaGetHeadersForContext(
  BuildContext context,
  String url,
) {
  final container = ProviderScope.containerOf(context, listen: false);
  return container.read(mediaGetAuthServiceProvider).headersFor(url);
}

String? extractServerAuthority(String baseUrl) {
  final uri = Uri.parse(baseUrl);
  if (uri.host.isEmpty) return null;
  final host = uri.host.contains(':') ? '[${uri.host}]' : uri.host;
  final port = uri.hasPort ? uri.port : null;
  final authority = port == null ? host : '$host:$port';
  return _normalizeAuthority(authority);
}

String _normalizeAuthority(String authority) {
  var normalized = authority.trim().toLowerCase();
  if (normalized.endsWith('.')) {
    normalized = normalized.substring(0, normalized.length - 1);
  }
  if (normalized.endsWith(':443')) {
    return normalized.substring(0, normalized.length - ':443'.length);
  }
  if (normalized.endsWith(':80')) {
    return normalized.substring(0, normalized.length - ':80'.length);
  }
  return normalized;
}
