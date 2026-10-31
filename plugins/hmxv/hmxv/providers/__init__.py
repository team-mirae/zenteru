"""LLM provider adapters with native tool calling."""

from __future__ import annotations

from .anthropic import AnthropicProvider
from .base import HTTPTransport, Provider, ProviderError, ToolCall, ToolSpec, TurnResult
from .gemini import GeminiProvider
from .openai_chat import ChatCompletionsProvider
from .openai_responses import ResponsesProvider

APIS = {
    "anthropic": AnthropicProvider,
    "responses": ResponsesProvider,
    "chat": ChatCompletionsProvider,
    "gemini": GeminiProvider,
}


def make_provider(settings, transport: HTTPTransport | None = None, stream: bool = True) -> Provider:
    cls = APIS.get(settings.api)
    if cls is None:
        raise ProviderError(f"unknown api {settings.api!r} for provider {settings.name}")
    return cls(settings, transport=transport, stream=stream)


__all__ = ["make_provider", "Provider", "ProviderError", "ToolCall", "ToolSpec", "TurnResult",
           "HTTPTransport", "AnthropicProvider", "ResponsesProvider", "ChatCompletionsProvider",
           "GeminiProvider"]
