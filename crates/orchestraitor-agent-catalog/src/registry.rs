//! Built-in domain and role registry data.

/// A built-in technical specialty from spec §9.19.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DomainDefinition {
    /// Stable domain identifier used in configuration and events.
    pub id: &'static str,
    /// Human-readable description of the domain's advisory scope.
    pub description: &'static str,
    /// Whether this domain is the required generic fallback.
    pub fallback: bool,
    /// Whether this domain is analysis-only and never an enforcement authority.
    pub analysis_only: bool,
}

/// A built-in work phase from spec §9.19.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleDefinition {
    /// Stable role identifier used in configuration and events.
    pub id: &'static str,
    /// Human-readable description of the role's work phase.
    pub description: &'static str,
}

/// The eight MVP built-in domains from spec §9.19.1.
pub const BUILT_IN_DOMAINS: [DomainDefinition; 8] = [
    DomainDefinition {
        id: "general",
        description: "Required generic fallback. Every project has it.",
        fallback: true,
        analysis_only: false,
    },
    DomainDefinition {
        id: "frontend",
        description: "Web frontend, styling, accessibility, browser runtimes.",
        fallback: false,
        analysis_only: false,
    },
    DomainDefinition {
        id: "backend",
        description: "Server, services, APIs, persistence, message buses.",
        fallback: false,
        analysis_only: false,
    },
    DomainDefinition {
        id: "data",
        description: "Pipelines, schemas, migrations, analytics, ML serving.",
        fallback: false,
        analysis_only: false,
    },
    DomainDefinition {
        id: "devops",
        description: "CI/CD, infrastructure, packaging, release engineering.",
        fallback: false,
        analysis_only: false,
    },
    DomainDefinition {
        id: "testing",
        description: "Test design, fixtures, property tests, regression suites.",
        fallback: false,
        analysis_only: false,
    },
    DomainDefinition {
        id: "documentation",
        description: "Prose, reference, examples, README, ADRs.",
        fallback: false,
        analysis_only: false,
    },
    DomainDefinition {
        id: "security",
        description: "Security analysis and guidance. Analysis only — never enforcement.",
        fallback: false,
        analysis_only: true,
    },
];

/// The five MVP built-in roles from spec §9.19.1.
pub const BUILT_IN_ROLES: [RoleDefinition; 5] = [
    RoleDefinition {
        id: "planning",
        description: "Producing or reviewing a work plan.",
    },
    RoleDefinition {
        id: "implementing",
        description: "Producing or modifying code.",
    },
    RoleDefinition {
        id: "reviewing",
        description: "Critiquing existing code or a diff.",
    },
    RoleDefinition {
        id: "testing",
        description: "Designing or running tests.",
    },
    RoleDefinition {
        id: "researching",
        description: "Gathering context from a local codebase or external docs.",
    },
];
