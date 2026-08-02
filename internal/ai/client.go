package ai

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/rselbach/argmax/internal/config"
)

// maxResponseBytes bounds provider response bodies at 1 MiB (AI-014).
const maxResponseBytes = 1 << 20

// errRateLimited marks an HTTP 429 from the provider (AI-013).
var errRateLimited = errors.New("ai: provider rate limited (HTTP 429)")

// chatResponse is the subset of the OpenAI chat-completions response we read.
type chatResponse struct {
	Choices []struct {
		Message struct {
			Content string `json:"content"`
		} `json:"message"`
	} `json:"choices"`
}

// chat performs one chat-completions request and returns the raw content of
// the first choice. The caller's ctx already carries the provider timeout;
// chat re-applies it so the call is bounded even when invoked directly.
//
// The API key and header values are never logged or included in errors
// (DIAG-008).
func (e *Engine) chat(ctx context.Context, p config.AIProvider, messages []Message) (string, error) {
	// Append /chat/completions unless the endpoint already carries it (AI-004).
	endpoint := strings.TrimRight(p.Endpoint, "/")
	if !strings.HasSuffix(endpoint, "/chat/completions") {
		endpoint += "/chat/completions"
	}

	// Defaults first, then overlay extra request-body fields so they may
	// override model parameters intentionally (AI-002, AI-014).
	body := map[string]any{
		"model":    p.Model,
		"messages": messages,
		"stream":   false,
	}
	for k, v := range p.ExtraRequestBody {
		body[k] = v
	}
	payload, err := json.Marshal(body)
	if err != nil {
		return "", err
	}

	timeout := time.Duration(p.TimeoutMs) * time.Millisecond
	if timeout <= 0 {
		timeout = defaultTimeoutMs * time.Millisecond
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(payload))
	if err != nil {
		return "", err
	}
	req.Header.Set("Content-Type", "application/json")
	if key := p.Key(); key != "" {
		req.Header.Set("Authorization", "Bearer "+key)
	}

	resp, err := e.httpClient.Do(req)
	if err != nil {
		return "", err
	}
	defer func() { _ = resp.Body.Close() }()

	if resp.StatusCode == http.StatusTooManyRequests {
		return "", errRateLimited
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return "", fmt.Errorf("ai: unexpected provider status %d", resp.StatusCode)
	}

	data, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBytes))
	if err != nil {
		return "", err
	}
	var parsed chatResponse
	if err := json.Unmarshal(data, &parsed); err != nil {
		return "", err
	}
	if len(parsed.Choices) == 0 {
		return "", errors.New("ai: provider returned no choices")
	}
	return parsed.Choices[0].Message.Content, nil
}
