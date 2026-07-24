import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart' as http_testing;
import 'package:image_picker/image_picker.dart';
import 'package:buzz/shared/relay/media_upload.dart';

const _apiKey = 'buzzk_upload_key';

final _pngBytes = Uint8List.fromList([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, //
  0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
]);

final _jpegBytes = Uint8List.fromList([
  0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x01, //
]);

final _heicBytes = Uint8List.fromList([
  0x00, 0x00, 0x00, 0x18, 0x66, 0x74, 0x79, 0x70, //
  0x68, 0x65, 0x69, 0x63, 0x00, 0x00, 0x00, 0x00,
  0x6d, 0x69, 0x66, 0x31, 0x68, 0x65, 0x69, 0x63,
]);

final _gifBytes = Uint8List.fromList([
  0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, //
]);

final _apngBytes = Uint8List.fromList([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, //
  0x00, 0x00, 0x00, 0x08, 0x61, 0x63, 0x54, 0x4c,
  0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
  0x00, 0x00, 0x00, 0x00,
]);

final _animatedWebpBytes = Uint8List.fromList([
  0x52, 0x49, 0x46, 0x46, 0x16, 0x00, 0x00, 0x00, //
  0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x58,
  0x0a, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
]);

const _mediaUploadPlatformChannel = MethodChannel('buzz/media_upload');

void _setMockMediaUploadPlatformHandler(
  Future<Object?> Function(MethodCall call)? handler,
) {
  TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
      .setMockMethodCallHandler(_mediaUploadPlatformChannel, handler);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(() {
    _setMockMediaUploadPlatformHandler((call) async {
      switch (call.method) {
        case 'sanitizeImageForUpload':
          final arguments = call.arguments as Map<Object?, Object?>;
          return arguments['bytes'] as Uint8List;
        case 'transcodeImageToJpeg':
          return _jpegBytes;
        default:
          return null;
      }
    });
  });

  tearDownAll(() {
    _setMockMediaUploadPlatformHandler(null);
  });

  group('MediaUploadService bearer auth', () {
    test('uploads gallery image bytes with a bearer token', () async {
      http.Request? capturedRequest;
      final client = http_testing.MockClient((request) async {
        capturedRequest = request;
        return http.Response(
          jsonEncode({
            'url': 'https://relay.example/media/test.png',
            'sha256':
                '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',
            'size': 16,
            'type': 'image/png',
            'uploaded': 1,
            'thumb': 'https://relay.example/media/test.thumb.jpg',
          }),
          200,
        );
      });

      final service = MediaUploadService(
        baseUrl: 'https://relay.example:8443',
        apiKey: _apiKey,
        httpClient: client,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async =>
            XFile.fromData(_pngBytes, name: 'tiny.png'),
      );

      final descriptor = await service.pickAndUploadImage();

      expect(descriptor, isNotNull);
      expect(descriptor!.type, 'image/png');
      expect(capturedRequest, isNotNull);
      expect(capturedRequest!.method, 'PUT');
      expect(
        capturedRequest!.url.toString(),
        'https://relay.example:8443/upload',
      );
      expect(capturedRequest!.headers['Content-Type'], 'image/png');
      expect(capturedRequest!.headers['X-SHA-256'], isNotEmpty);
      expect(capturedRequest!.headers['Authorization'], 'Bearer $_apiKey');
      // Server authors the row from a bearer identity — no Blossom/NIP-98 event.
      expect(
        capturedRequest!.headers['Authorization'],
        isNot(startsWith('Nostr ')),
      );
      expect(capturedRequest!.bodyBytes, _pngBytes);
    });

    test('refuses to upload without an API key', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: null,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async => null,
      );

      await expectLater(
        service.uploadBytes(_pngBytes, mimeType: 'image/png'),
        throwsA(
          isA<Exception>().having(
            (error) => error.toString(),
            'message',
            contains('no API key'),
          ),
        ),
      );
    });

    test(
      'retries the legacy upload route when the standard route is absent',
      () async {
        final requests = <http.Request>[];
        final client = http_testing.MockClient((request) async {
          requests.add(request);
          if (request.url.path == '/upload') {
            return http.Response('not found', HttpStatus.notFound);
          }
          return http.Response(
            jsonEncode({
              'url': 'https://relay.example/media/test.png',
              'sha256': request.headers['X-SHA-256'],
              'size': _pngBytes.length,
              'type': 'image/png',
              'uploaded': 1,
            }),
            200,
          );
        });
        final service = MediaUploadService(
          baseUrl: 'https://relay.example',
          apiKey: _apiKey,
          httpClient: client,
          pickGalleryVideo: () async => null,
          pickGalleryImage: () async => null,
        );

        await service.uploadBytes(_pngBytes, mimeType: 'image/png');

        expect(requests.map((request) => request.url.path), [
          '/upload',
          '/media/upload',
        ]);
        expect(requests[1].bodyBytes, requests[0].bodyBytes);
        expect(requests[0].headers['Authorization'], 'Bearer $_apiKey');
        expect(requests[1].headers['Authorization'], 'Bearer $_apiKey');
        expect(
          requests[1].headers['X-SHA-256'],
          requests[0].headers['X-SHA-256'],
        );
      },
    );

    for (final statusCode in [
      HttpStatus.unsupportedMediaType,
      HttpStatus.unprocessableEntity,
    ]) {
      test(
        'maps $statusCode media policy responses to friendly copy',
        () async {
          final service = MediaUploadService(
            baseUrl: 'https://relay.example',
            apiKey: _apiKey,
            httpClient: http_testing.MockClient(
              (request) async => http.Response(
                '{"error":"media contains metadata"}',
                statusCode,
              ),
            ),
            pickGalleryVideo: () async => null,
            pickGalleryImage: () async => null,
          );

          await expectLater(
            service.uploadBytes(_pngBytes, mimeType: 'image/png'),
            throwsA(
              isA<MediaPolicyUploadException>().having(
                (error) => error.toString(),
                'message',
                "We couldn't prepare this image for upload.",
              ),
            ),
          );
        },
      );
    }

    test('preserves video policy response details', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: _apiKey,
        httpClient: http_testing.MockClient(
          (request) async => http.Response(
            '{"error":"unsupported video codec"}',
            HttpStatus.unprocessableEntity,
          ),
        ),
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async => null,
      );

      await expectLater(
        service.uploadBytes(Uint8List(0), mimeType: 'video/mp4'),
        throwsA(
          isA<Exception>()
              .having(
                (error) => error,
                'type',
                isNot(isA<MediaPolicyUploadException>()),
              )
              .having(
                (error) => error.toString(),
                'message',
                contains('unsupported video codec'),
              ),
        ),
      );
    });
  });

  group('MediaUploadService clipboard', () {
    test(
      'checks clipboard image availability through the platform channel',
      () async {
        final invokedMethods = <String>[];
        _setMockMediaUploadPlatformHandler((call) async {
          invokedMethods.add(call.method);
          if (call.method == 'clipboardHasImage') return true;
          return null;
        });
        addTearDown(_restoreDefaultPlatformHandler);
        final service = MediaUploadService(
          baseUrl: 'https://relay.example',
          apiKey: null,
          pickGalleryVideo: () async => null,
          pickGalleryImage: () async => null,
        );

        expect(await service.clipboardHasImage(), isTrue);
        expect(invokedMethods, ['clipboardHasImage']);
      },
    );

    test('reads clipboard image through the platform channel', () async {
      final invokedMethods = <String>[];
      _setMockMediaUploadPlatformHandler((call) async {
        invokedMethods.add(call.method);
        if (call.method == 'readClipboardImage') return _pngBytes;
        if (call.method == 'sanitizeImageForUpload') {
          final arguments = call.arguments as Map<Object?, Object?>;
          return arguments['bytes'] as Uint8List;
        }
        return null;
      });
      addTearDown(_restoreDefaultPlatformHandler);
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: _apiKey,
        httpClient: http_testing.MockClient(
          (request) async => http.Response(
            jsonEncode({
              'url': 'https://relay.example/media/clipboard.png',
              'sha256':
                  '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',
              'size': 16,
              'type': 'image/png',
              'uploaded': 1,
            }),
            200,
          ),
        ),
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async => null,
      );

      final descriptor = await service.readAndUploadClipboardImage();

      expect(invokedMethods.first, 'readClipboardImage');
      expect(descriptor.type, 'image/png');
    });

    test(
      'rejects GIF clipboard bytes through the shared validation path',
      () async {
        final service = MediaUploadService(
          baseUrl: 'https://relay.example',
          apiKey: null,
          pickGalleryVideo: () async => null,
          pickGalleryImage: () async => null,
          readClipboardImage: () async => _gifBytes,
        );

        expect(
          service.readAndUploadClipboardImage,
          throwsA(
            isA<Exception>().having(
              (error) => error.toString(),
              'message',
              contains('GIF uploads are not supported on mobile yet'),
            ),
          ),
        );
      },
    );

    test('rejects empty clipboard image bytes', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: null,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async => null,
        readClipboardImage: () async => Uint8List(0),
      );

      expect(
        service.readAndUploadClipboardImage,
        throwsA(
          isA<Exception>().having(
            (error) => error.toString(),
            'message',
            contains('Unable to read pasted image'),
          ),
        ),
      );
    });
  });

  group('MediaUploadService image preparation', () {
    test('returns null when the gallery picker is cancelled', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: null,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async => null,
      );

      final result = await service.pickAndUploadImage();
      expect(result, isNull);
    });

    test('transcodes HEIC gallery files on iOS before upload', () async {
      final previousPlatform = debugDefaultTargetPlatformOverride;
      debugDefaultTargetPlatformOverride = TargetPlatform.iOS;
      addTearDown(() {
        debugDefaultTargetPlatformOverride = previousPlatform;
      });

      Uint8List? transcodedInput;
      http.Request? capturedRequest;
      final client = http_testing.MockClient((request) async {
        capturedRequest = request;
        return http.Response(
          jsonEncode({
            'url': 'https://relay.example/media/test.jpg',
            'sha256':
                'fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210',
            'size': _jpegBytes.length,
            'type': 'image/jpeg',
            'uploaded': 1,
          }),
          200,
        );
      });

      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: _apiKey,
        httpClient: client,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async =>
            XFile.fromData(_heicBytes, name: 'photo.heic'),
        transcodeImageToJpeg: (bytes) async {
          transcodedInput = bytes;
          return _jpegBytes;
        },
      );

      final descriptor = await service.pickAndUploadImage();

      expect(descriptor, isNotNull);
      expect(descriptor!.type, 'image/jpeg');
      expect(transcodedInput, _heicBytes);
      expect(capturedRequest!.headers['Content-Type'], 'image/jpeg');
      expect(capturedRequest!.bodyBytes, _jpegBytes);
    });

    test('sanitizes iOS JPEG gallery files before upload', () async {
      final previousPlatform = debugDefaultTargetPlatformOverride;
      debugDefaultTargetPlatformOverride = TargetPlatform.iOS;
      addTearDown(() {
        debugDefaultTargetPlatformOverride = previousPlatform;
      });

      Uint8List? sanitizedInput;
      String? sanitizedMimeType;
      http.Request? capturedRequest;
      final client = http_testing.MockClient((request) async {
        capturedRequest = request;
        return http.Response(
          jsonEncode({
            'url': 'https://relay.example/media/test.jpg',
            'sha256':
                'abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd',
            'size': _jpegBytes.length,
            'type': 'image/jpeg',
            'uploaded': 1,
          }),
          200,
        );
      });

      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: _apiKey,
        httpClient: client,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async =>
            XFile.fromData(_jpegBytes, name: 'photo.jpg'),
        sanitizeImageBytes: (bytes, mimeType) async {
          sanitizedInput = bytes;
          sanitizedMimeType = mimeType;
          return _jpegBytes;
        },
      );

      final descriptor = await service.pickAndUploadImage();

      expect(descriptor!.type, 'image/jpeg');
      expect(sanitizedInput, _jpegBytes);
      expect(sanitizedMimeType, 'image/jpeg');
      expect(capturedRequest!.bodyBytes, _jpegBytes);
    });

    test('rejects GIF gallery files before upload', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: _apiKey,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async =>
            XFile.fromData(_gifBytes, name: 'animated.gif'),
      );

      expect(
        service.pickAndUploadImage(),
        throwsA(
          isA<Exception>().having(
            (error) => error.toString(),
            'message',
            contains('GIF uploads are not supported on mobile yet'),
          ),
        ),
      );
    });

    test('rejects animated PNG gallery files before upload', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: _apiKey,
        httpClient: http_testing.MockClient(
          (request) async => http.Response('{}', 200),
        ),
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async =>
            XFile.fromData(_apngBytes, name: 'animated.png'),
      );

      expect(
        service.pickAndUploadImage(),
        throwsA(
          isA<Exception>().having(
            (error) => error.toString(),
            'message',
            contains('Animated PNG uploads are not supported on mobile yet'),
          ),
        ),
      );
    });

    test('rejects animated WebP gallery files before upload', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: _apiKey,
        httpClient: http_testing.MockClient(
          (request) async => http.Response('{}', 200),
        ),
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async =>
            XFile.fromData(_animatedWebpBytes, name: 'animated.webp'),
      );

      expect(
        service.pickAndUploadImage(),
        throwsA(
          isA<Exception>().having(
            (error) => error.toString(),
            'message',
            contains('Animated WebP uploads are not supported on mobile yet'),
          ),
        ),
      );
    });

    test('rejects unsupported gallery files before upload', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: _apiKey,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async => XFile.fromData(
          Uint8List.fromList(utf8.encode('not an image')),
          name: 'note.txt',
        ),
      );

      expect(
        service.pickAndUploadImage(),
        throwsA(
          isA<Exception>().having(
            (error) => error.toString(),
            'message',
            contains('unsupported file type'),
          ),
        ),
      );
    });
  });

  group('pickAndUploadVideo', () {
    Uint8List buildFtypHeader(String brand) {
      final bytes = Uint8List(32);
      bytes[3] = 32;
      bytes[4] = 0x66;
      bytes[5] = 0x74;
      bytes[6] = 0x79;
      bytes[7] = 0x70;
      final brandBytes = ascii.encode(brand);
      for (var i = 0; i < 4 && i < brandBytes.length; i++) {
        bytes[8 + i] = brandBytes[i];
      }
      return bytes;
    }

    Future<(XFile, File)> writeTempVideo(Uint8List bytes, String name) async {
      final dir = await Directory.systemTemp.createTemp('video_test_');
      final file = File('${dir.path}/$name');
      await file.writeAsBytes(bytes);
      return (XFile(file.path), file);
    }

    test('rebuilds an existing MP4 container before upload', () async {
      var transcodeCalled = false;
      http.Request? capturedRequest;
      final client = http_testing.MockClient((request) async {
        capturedRequest = request;
        return http.Response(
          jsonEncode({
            'url': 'https://relay.example/media/test.mp4',
            'sha256':
                '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef',
            'size': 32,
            'type': 'video/mp4',
            'uploaded': 1,
          }),
          200,
        );
      });

      final mp4Bytes = buildFtypHeader('isom');
      final (xfile, tempFile) = await writeTempVideo(mp4Bytes, 'clip.mp4');
      try {
        final service = MediaUploadService(
          baseUrl: 'https://relay.example',
          apiKey: _apiKey,
          httpClient: client,
          pickGalleryVideo: () async => xfile,
          pickGalleryImage: () async => null,
          transcodeVideoToMp4: (path) async {
            transcodeCalled = true;
            final outDir = await Directory.systemTemp.createTemp('transcode_');
            final outFile = File('${outDir.path}/out.mp4');
            await outFile.writeAsBytes(buildFtypHeader('isom'));
            return outFile.path;
          },
        );

        final descriptor = await service.pickAndUploadVideo();
        expect(descriptor!.type, 'video/mp4');
        expect(transcodeCalled, isTrue);
        expect(capturedRequest!.headers['Content-Type'], 'video/mp4');
        expect(capturedRequest!.headers['Authorization'], 'Bearer $_apiKey');
      } finally {
        await tempFile.parent.delete(recursive: true);
      }
    });

    test('returns null when video picker is cancelled', () async {
      final service = MediaUploadService(
        baseUrl: 'https://relay.example',
        apiKey: null,
        pickGalleryVideo: () async => null,
        pickGalleryImage: () async => null,
      );

      final result = await service.pickAndUploadVideo();
      expect(result, isNull);
    });

    test('rejects videos over 100MB', () async {
      final dir = await Directory.systemTemp.createTemp('video_size_test_');
      final file = File('${dir.path}/huge.mp4');
      final raf = await file.open(mode: FileMode.write);
      await raf.truncate(101 * 1024 * 1024);
      await raf.close();

      try {
        final service = MediaUploadService(
          baseUrl: 'https://relay.example',
          apiKey: null,
          pickGalleryVideo: () async => XFile(file.path),
          pickGalleryImage: () async => null,
        );

        await expectLater(
          () => service.pickAndUploadVideo(),
          throwsA(
            isA<Exception>().having(
              (e) => e.toString(),
              'message',
              contains('too large'),
            ),
          ),
        );
      } finally {
        await dir.delete(recursive: true);
      }
    });
  });
}

void _restoreDefaultPlatformHandler() {
  _setMockMediaUploadPlatformHandler((call) async {
    switch (call.method) {
      case 'sanitizeImageForUpload':
        final arguments = call.arguments as Map<Object?, Object?>;
        return arguments['bytes'] as Uint8List;
      case 'transcodeImageToJpeg':
        return _jpegBytes;
      default:
        return null;
    }
  });
}
