// Package ai implements the optional OpenAI-compatible completion engine:
// provider client, bounded context gathering, prompt construction, response
// validation, caching, cancellation, and rate-limit cooldown. While AI is
// disabled no endpoint is contacted and no context is gathered.
package ai

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"

	"github.com/rselbach/argmax/internal/config"
)

const maxResponseBody = 1 << 20

// Client speaks the OpenAI-compatible chat-completions protocol to one
// named provider.
type Client struct {
	provider *config.Provider
	http     *http.Client
}

// NewClient returns a client for the given provider.
func NewClient(p *config.Provider) *Client {
	return &Client{provider: p, http: &http.Client{Timeout: p.Timeout()}}
}

// ErrRateLimited marks HTTP 429 responses so callers can enter cooldown.
var ErrRateLimited = fmt.Errorf("provider rate limited")

// Complete sends the prompt pair and returns the raw model output.
func (c *Client) Complete(ctx context.Context, system, user string) (string, error) {
	endpoint := strings.TrimRight(c.provider.Endpoint, "/")
	if !strings.HasSuffix(endpoint, "/chat/completions") {
		endpoint += "/chat/completions"
	}
	body := map[string]any{
		"model": c.provider.Model,
		"messages": []map[string]string{
			{"role": "system", "content": system},
			{"role": "user", "content": user},
		},
		"temperature": 0.2,
		"max_tokens":  120,
		"stream":      false,
	}
	// Extra fields may intentionally override default model parameters.
	for k, v := range c.provider.ExtraRequestBody {
		body[k] = v
	}
	payload, err := json.Marshal(body)
	if err != nil {
		return "", fmt.Errorf("encode request: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(payload))
	if err != nil {
		return "", fmt.Errorf("build request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	if key := c.provider.ResolveAPIKey(); key != "" {
		req.Header.Set("Authorization", "Bearer "+key)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return "", fmt.Errorf("provider request: %w", err)
	}
	defer func() { _ = resp.Body.Close() }()
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxResponseBody))
	if err != nil {
		return "", fmt.Errorf("read response: %w", err)
	}
	if resp.StatusCode == http.StatusTooManyRequests {
		return "", ErrRateLimited
	}
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("provider returned HTTP %d", resp.StatusCode)
	}
	var parsed struct {
		Choices []struct {
			Message struct {
				Content string `json:"content"`
			} `json:"message"`
		} `json:"choices"`
	}
	if err := json.Unmarshal(data, &parsed); err != nil {
		return "", fmt.Errorf("decode response: %w", err)
	}
	if len(parsed.Choices) == 0 {
		return "", fmt.Errorf("provider returned no choices")
	}
	return parsed.Choices[0].Message.Content, nil
}
