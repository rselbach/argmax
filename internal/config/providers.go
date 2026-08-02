package config

import "fmt"

// openAIBase is the well-known protocol base providers may inherit from
// without defining it: OpenAI-compatible chat completions.
const openAIBase = "openai"

// resolveProviderInheritance fills empty provider fields from the
// inherited_from base. A named base must exist unless it is the built-in
// "openai" protocol marker; cycles are rejected.
func resolveProviderInheritance(cfg *Config) error {
	resolved := map[string]bool{}
	var resolve func(name string, chain map[string]bool) error
	resolve = func(name string, chain map[string]bool) error {
		if resolved[name] {
			return nil
		}
		p := cfg.AI.Providers[name]
		base := p.InheritedFrom
		if base == "" {
			resolved[name] = true
			return nil
		}
		if chain[name] {
			return keyError("ai.providers."+name+".inherited_from", "inheritance must not form a cycle")
		}
		chain[name] = true
		bp, ok := cfg.AI.Providers[base]
		switch {
		case ok:
			if err := resolve(base, chain); err != nil {
				return err
			}
			bp = cfg.AI.Providers[base]
			if p.Endpoint == "" {
				p.Endpoint = bp.Endpoint
			}
			if p.Model == "" {
				p.Model = bp.Model
			}
			if p.APIKey == "" {
				p.APIKey = bp.APIKey
			}
			if p.APIKeyEnv == "" {
				p.APIKeyEnv = bp.APIKeyEnv
			}
			if p.TimeoutMS == 0 {
				p.TimeoutMS = bp.TimeoutMS
			}
			if len(bp.ExtraRequestBody) > 0 {
				merged := make(map[string]any, len(bp.ExtraRequestBody)+len(p.ExtraRequestBody))
				for k, v := range bp.ExtraRequestBody {
					merged[k] = v
				}
				for k, v := range p.ExtraRequestBody {
					merged[k] = v
				}
				p.ExtraRequestBody = merged
			}
		case base == openAIBase:
			// Protocol marker: apply the OpenAI defaults for fields the
			// provider leaves unset.
			if p.Endpoint == "" {
				p.Endpoint = "https://api.openai.com/v1"
			}
			if p.APIKeyEnv == "" && p.APIKey == "" {
				p.APIKeyEnv = "OPENAI_API_KEY"
			}
		default:
			return keyError("ai.providers."+name+".inherited_from",
				fmt.Sprintf("base provider %q is not configured (only %q is built in)", base, openAIBase))
		}
		cfg.AI.Providers[name] = p
		resolved[name] = true
		return nil
	}
	for name := range cfg.AI.Providers {
		if err := resolve(name, map[string]bool{}); err != nil {
			return err
		}
	}
	return nil
}
