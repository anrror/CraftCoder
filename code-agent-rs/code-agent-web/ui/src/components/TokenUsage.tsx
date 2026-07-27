interface TokenUsageProps {
  promptTokens?: number;
  completionTokens?: number;
  totalTokens?: number;
}

export default function TokenUsage({ promptTokens, completionTokens, totalTokens }: TokenUsageProps) {
  const parts: string[] = [];

  if (promptTokens !== undefined) {
    parts.push(`<span class="token-stat"><span class="token-label">IN</span> ${promptTokens}</span>`);
  }
  if (completionTokens !== undefined) {
    parts.push(`<span class="token-stat"><span class="token-label">OUT</span> ${completionTokens}</span>`);
  }
  if (totalTokens !== undefined) {
    parts.push(`<span class="token-stat"><span class="token-label">TOTAL</span> ${totalTokens}</span>`);
  }

  if (parts.length === 0) {
    parts.push('<span class="token-stat">Tokens used</span>');
  }

  return (
    <div
      className="token-usage"
      dangerouslySetInnerHTML={{ __html: parts.join('') }}
    />
  );
}
