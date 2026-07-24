import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../../shared/relay/relay.dart';
import 'observer_models.dart';
import 'transcript_builder.dart';

/// Maximum observer events to keep per agent.
const _maxObserverEvents = 800;

/// Key for channel-scoped transcript reads.
typedef ObserverKey = ({String channelId, String agentPubkey});

/// State emitted by the channel-scoped observer transcript provider.
@immutable
class ObserverState {
  final ObserverConnectionState connection;
  final List<TranscriptItem> transcript;
  final String? errorMessage;

  const ObserverState({
    required this.connection,
    required this.transcript,
    this.errorMessage,
  });

  const ObserverState.initial()
    : connection = ObserverConnectionState.idle,
      transcript = const [],
      errorMessage = null;
}

@immutable
class ObserverRelayState {
  final ObserverConnectionState connection;
  final Map<String, List<ObserverFrame>> framesByAgent;
  final String? errorMessage;

  const ObserverRelayState({
    required this.connection,
    required this.framesByAgent,
    this.errorMessage,
  });

  const ObserverRelayState.initial()
    : connection = ObserverConnectionState.idle,
      framesByAgent = const {},
      errorMessage = null;
}

class ObserverRelayNotifier extends Notifier<ObserverRelayState> {
  final Map<String, List<ObserverFrame>> _framesByAgent = {};
  final Map<String, Set<String>> _dedupeKeysByAgent = {};

  void Function()? _unsubscribe;
  Future<void>? _startFuture;
  String? _ownerPubkey;
  String? _identityKey;
  String? _errorMessage;
  int _subscriptionEpoch = 0;
  bool _disposed = false;

  @override
  ObserverRelayState build() {
    final config = ref.watch(relayConfigProvider);
    final sessionState = ref.watch(relaySessionProvider);
    final identityKey = '${config.baseUrl}|${config.actorPubkey ?? ''}';

    _disposed = false;
    if (_identityKey != null && _identityKey != identityKey) {
      _reset();
    }
    _identityKey = identityKey;

    ref.onDispose(() {
      _disposed = true;
      _reset();
    });

    if (sessionState.status == SessionStatus.connected) {
      Future.microtask(_ensureSubscribed);
    }

    final hasActor = config.actorPubkey?.isNotEmpty == true;
    return ObserverRelayState(
      connection: hasActor
          ? _connectionForSession(sessionState.status)
          : ObserverConnectionState.idle,
      framesByAgent: _snapshotFrames(),
      errorMessage: _errorMessage,
    );
  }

  Future<void> _ensureSubscribed() {
    if (_disposed) return Future.value();
    if (_unsubscribe != null) return Future.value();
    if (_startFuture != null) return _startFuture!;

    _startFuture = _subscribe(_subscriptionEpoch);
    return _startFuture!;
  }

  Future<void> _subscribe(int epoch) async {
    try {
      if (_disposed || epoch != _subscriptionEpoch) {
        return;
      }

      final config = ref.read(relayConfigProvider);
      final actor = config.actorPubkey?.trim();
      if (actor == null || actor.isEmpty) {
        _errorMessage = null;
        _emit(connection: ObserverConnectionState.idle);
        return;
      }

      final ownerPubkey = actor.toLowerCase();
      if (_disposed || epoch != _subscriptionEpoch) {
        return;
      }
      _ownerPubkey = ownerPubkey;
      _errorMessage = null;
      _emit(connection: ObserverConnectionState.connecting);

      final session = ref.read(relaySessionProvider.notifier);
      final unsubscribe = await session.subscribe(
        NostrFilter(
          kinds: [EventKind.agentObserverFrame],
          tags: {
            '#p': [ownerPubkey],
          },
          limit: 0,
        ),
        _handleEvent,
        onClosed: (message) {
          _unsubscribe = null;
          _errorMessage = 'Observer subscription closed: $message';
          _emit(connection: ObserverConnectionState.error);
        },
      );

      if (_disposed || epoch != _subscriptionEpoch) {
        unsubscribe();
        return;
      }

      _unsubscribe = unsubscribe;
      _emit(connection: ObserverConnectionState.open);
    } catch (error) {
      if (_disposed || epoch != _subscriptionEpoch) {
        return;
      }
      _errorMessage = _observerErrorMessage(error);
      _emit(connection: ObserverConnectionState.error);
    } finally {
      if (epoch == _subscriptionEpoch) {
        _startFuture = null;
      }
    }
  }

  void _handleEvent(NostrEvent event) {
    final agentPubkey = event.getTagValue('agent');
    if (agentPubkey == null || event.getTagValue('frame') != 'telemetry') {
      return;
    }

    final normalizedAgent = agentPubkey.toLowerCase();
    if (event.pubkey.toLowerCase() != normalizedAgent) {
      return;
    }

    final ownerPubkey = _ownerPubkey;
    if (ownerPubkey == null) {
      return;
    }

    final pTag = event.getTagValue('p');
    if (pTag?.toLowerCase() != ownerPubkey.toLowerCase()) {
      return;
    }

    final frame = _parseFrame(event);
    if (frame == null) return;

    final dedupeKey = '${frame.seq}:${frame.timestamp}';
    final dedupeKeys = _dedupeKeysByAgent.putIfAbsent(
      normalizedAgent,
      () => <String>{},
    );
    if (!dedupeKeys.add(dedupeKey)) {
      return;
    }

    final frames = _framesByAgent.putIfAbsent(
      normalizedAgent,
      () => <ObserverFrame>[],
    );
    frames.add(frame);
    frames.sort(_compareObserverFrames);

    if (frames.length > _maxObserverEvents) {
      final removeCount = frames.length - _maxObserverEvents;
      for (final removed in frames.take(removeCount)) {
        dedupeKeys.remove('${removed.seq}:${removed.timestamp}');
      }
      frames.removeRange(0, removeCount);
    }

    _errorMessage = null;
    _emit(connection: ObserverConnectionState.open);
  }

  ObserverFrame? _parseFrame(NostrEvent event) {
    // Observer frames are now server-side plaintext (E2E dropped in the
    // API-key migration); the relay gates them by scope + `#p` membership.
    try {
      final json = jsonDecode(event.content) as Map<String, dynamic>;
      return ObserverFrame.fromJson(json);
    } catch (error) {
      _errorMessage = 'Observer event parse failed: $error';
      _emit(connection: ObserverConnectionState.error);
      return null;
    }
  }

  void _emit({required ObserverConnectionState connection}) {
    if (_disposed) return;
    state = ObserverRelayState(
      connection: connection,
      framesByAgent: _snapshotFrames(),
      errorMessage: _errorMessage,
    );
  }

  Map<String, List<ObserverFrame>> _snapshotFrames() {
    return Map<String, List<ObserverFrame>>.unmodifiable({
      for (final entry in _framesByAgent.entries)
        entry.key: List<ObserverFrame>.unmodifiable(entry.value),
    });
  }

  void _reset() {
    _subscriptionEpoch += 1;
    _unsubscribe?.call();
    _unsubscribe = null;
    _startFuture = null;
    _ownerPubkey = null;
    _errorMessage = null;
    _framesByAgent.clear();
    _dedupeKeysByAgent.clear();
  }

  ObserverConnectionState _connectionForSession(SessionStatus status) {
    if (_errorMessage != null && _unsubscribe == null && _startFuture == null) {
      return ObserverConnectionState.error;
    }
    return switch (status) {
      SessionStatus.connected =>
        _unsubscribe == null
            ? ObserverConnectionState.connecting
            : ObserverConnectionState.open,
      SessionStatus.connecting ||
      SessionStatus.reconnecting => ObserverConnectionState.connecting,
      SessionStatus.disconnected => ObserverConnectionState.idle,
    };
  }

  static String _observerErrorMessage(Object error) {
    if (error is FormatException) {
      return error.message;
    }
    return 'Observer subscription failed: $error';
  }

  static int _compareObserverFrames(ObserverFrame a, ObserverFrame b) {
    final tsA = DateTime.tryParse(a.timestamp)?.millisecondsSinceEpoch ?? 0;
    final tsB = DateTime.tryParse(b.timestamp)?.millisecondsSinceEpoch ?? 0;
    if (tsA != tsB) return tsA.compareTo(tsB);
    return a.seq.compareTo(b.seq);
  }
}

final observerRelayProvider =
    NotifierProvider<ObserverRelayNotifier, ObserverRelayState>(
      ObserverRelayNotifier.new,
    );

final observerSubscriptionProvider =
    Provider.family<ObserverState, ObserverKey>((ref, key) {
      final relayState = ref.watch(observerRelayProvider);
      final normalizedAgent = key.agentPubkey.toLowerCase();
      final frames = relayState.framesByAgent[normalizedAgent] ?? const [];
      final channelFrames = [
        for (final frame in frames)
          if (frame.channelId == null || frame.channelId == key.channelId)
            frame,
      ];

      return ObserverState(
        connection: relayState.connection,
        transcript: buildTranscript(channelFrames),
        errorMessage: relayState.errorMessage,
      );
    });
