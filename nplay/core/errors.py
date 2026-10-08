class NPlayError(RuntimeError):pass
class ProviderError(NPlayError):pass
class PlaybackError(NPlayError):pass
class AuthenticationError(ProviderError):pass
class NetworkError(ProviderError):pass
class MediaUnavailableError(PlaybackError):pass
class ConfigurationError(NPlayError):pass
