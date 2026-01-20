# Monetization Strategy

## Revenue Models

### Model 1: Open Core SaaS

**Structure:**

- Core framework: Free (Apache 2.0)
- Teams features: Paid subscription
- Enterprise: Custom pricing

**Teams Tier ($20/seat/month):**

- Git-native memory sync
- Shared team context
- Analytics dashboard
- Priority support
- 14-day free trial

**Enterprise Tier (Custom):**

- On-premise deployment
- SSO/SAML integration
- Audit logging
- Dedicated support
- SLA guarantees
- Custom integrations

### Model 2: Dual License

**Structure:**

- Open source: Apache 2.0 (individuals, small teams)
- Commercial: Paid license (companies > N employees)

**Example (similar to Parity License):**

- Free for individuals and small teams (<5 people)
- $500/year for small companies (5-50 employees)
- $2,000/year for medium companies (50-500 employees)
- Custom for enterprise (500+ employees)

### Model 3: Support & Services

**Structure:**

- Everything open source
- Revenue from support contracts
- Professional services

**Support Tiers:**

- Community: Free (GitHub issues)
- Standard: $200/month (email, 24h response)
- Premium: $1,000/month (Slack, 4h response)
- Enterprise: Custom (dedicated engineer)

**Professional Services:**

- Implementation: $5,000-20,000
- Custom development: $200/hour
- Training: $2,000/day

## Pricing Research

### Comparable Products

| Product          | Pricing              | Model        |
| ---------------- | -------------------- | ------------ |
| Cursor           | $20/user/month       | Subscription |
| GitHub Copilot   | $19-39/user/month    | Subscription |
| Sourcegraph Cody | $9-49/user/month     | Subscription |
| Linear           | $8/user/month        | Subscription |
| Mem0             | $25-50/month + usage | Usage-based  |
| Letta Cloud      | Usage-based          | Consumption  |

### Price Sensitivity Analysis

| Segment         | Willingness to Pay | Notes            |
| --------------- | ------------------ | ---------------- |
| Individual devs | $0-20/month        | Price sensitive  |
| Small teams     | $10-30/seat/month  | Value-conscious  |
| Enterprise      | $50-200/seat/month | Budget available |

### Recommended Pricing

| Tier       | Price          | Target             |
| ---------- | -------------- | ------------------ |
| Free       | $0             | Individual devs    |
| Teams      | $20/seat/month | Small teams (5-50) |
| Enterprise | $75/seat/month | Large orgs (50+)   |

**Discounts:**

- Annual: 20% off
- Startups: 50% off (first year)
- Open source: Free Teams tier
- Education: 90% off

## Unit Economics

### Teams Tier ($20/seat/month)

**Revenue per seat:** $20/month

**Costs per seat:**
| Item | Cost | Notes |
|------|------|-------|
| Infrastructure | $2-3 | Sync, storage |
| Support | $2-3 | Amortized |
| Embeddings | $1-2 | If we provide |
| **Total COGS** | **$5-8** | |

**Gross margin:** 60-75%

### Enterprise Tier ($75/seat/month)

**Revenue per seat:** $75/month

**Costs per seat:**
| Item | Cost | Notes |
|------|------|-------|
| Infrastructure | $5 | On-prem support |
| Support | $10 | Dedicated |
| Sales | $15 | Amortized CAC |
| **Total COGS** | **$30** | |

**Gross margin:** 60%

## Revenue Projections

### Year 1 (Bootstrap)

| Segment    | Customers | Seats | MRR        | ARR         |
| ---------- | --------- | ----- | ---------- | ----------- |
| Teams      | 20        | 100   | $2,000     | $24,000     |
| Enterprise | 1         | 50    | $3,750     | $45,000     |
| **Total**  |           |       | **$5,750** | **$69,000** |

### Year 2 (Growth)

| Segment    | Customers | Seats | MRR         | ARR          |
| ---------- | --------- | ----- | ----------- | ------------ |
| Teams      | 100       | 500   | $10,000     | $120,000     |
| Enterprise | 5         | 500   | $37,500     | $450,000     |
| **Total**  |           |       | **$47,500** | **$570,000** |

### Year 3 (Scale)

| Segment    | Customers | Seats | MRR          | ARR            |
| ---------- | --------- | ----- | ------------ | -------------- |
| Teams      | 300       | 2,000 | $40,000      | $480,000       |
| Enterprise | 20        | 3,000 | $225,000     | $2,700,000     |
| **Total**  |           |       | **$265,000** | **$3,180,000** |

## Conversion Funnel

### OSS → Teams Conversion

**Target:** 2-5% of active OSS users convert to Teams

**Funnel:**

```
GitHub stars: 10,000
        ↓ (20% active)
Active users: 2,000
        ↓ (10% teams)
Team usage: 200
        ↓ (25% convert)
Paying teams: 50
        ↓ (5 seats avg)
Paying seats: 250
```

### Teams → Enterprise Conversion

**Target:** 5-10% of Teams customers upgrade to Enterprise

**Triggers:**

- Team size > 20
- Compliance requirements
- On-prem needs
- Support requirements

## What Teams Features Are Worth Paying For

### Must-Have (High Willingness to Pay)

1. **Shared team memory**
   - New team members inherit context
   - Team knowledge persists when people leave
   - "Team gets smarter as you use it"

2. **Git-native sync**
   - Memory syncs via git (existing workflow)
   - No new cloud service
   - Privacy-preserving

3. **Onboarding acceleration**
   - "Ask the agent about our codebase"
   - Reduces ramp-up time
   - Institutional knowledge

### Nice-to-Have (Moderate Willingness to Pay)

4. **Analytics dashboard**
   - What is the agent doing?
   - Team usage patterns
   - Memory growth over time

5. **Custom context strategies**
   - Team-specific retrieval tuning
   - Priority entities
   - Custom extractors

### Future Upsells

6. **Model routing optimization**
   - Automatic model selection
   - Cost optimization
   - Quality/cost tradeoff

7. **Advanced integrations**
   - Jira/Linear integration
   - Slack notifications
   - CI/CD integration

## Risks to Monetization

| Risk                            | Likelihood | Impact | Mitigation                  |
| ------------------------------- | ---------- | ------ | --------------------------- |
| No conversion from OSS          | Medium     | High   | Compelling Teams features   |
| Enterprise sales too slow       | High       | Medium | PLG-first, enterprise later |
| Price pressure from competitors | Medium     | Medium | Focus on value, not price   |
| Big player makes it free        | Medium     | High   | Build community moat        |

## Recommended Approach

1. **Start with OSS** - Build adoption, prove value
2. **Launch Teams early** - Validate willingness to pay
3. **Enterprise later** - Once Teams is working
4. **Iterate on pricing** - Based on real data

## Key Metrics to Track

| Metric               | Target Y1 | Target Y2 |
| -------------------- | --------- | --------- |
| GitHub stars         | 1,000     | 5,000     |
| Monthly active users | 500       | 2,000     |
| Teams customers      | 20        | 100       |
| Teams MRR            | $2,000    | $10,000   |
| Enterprise customers | 1         | 5         |
| Total ARR            | $69,000   | $570,000  |

## References

- [STRATEGY.md](STRATEGY.md) - Overall strategy
- [MARKET.md](MARKET.md) - Market analysis
- [POSITIONING.md](POSITIONING.md) - Value proposition
