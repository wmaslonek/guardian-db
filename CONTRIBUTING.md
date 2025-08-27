# Contribuindo para Guardian DB

Obrigado por considerar contribuir para o Guardian DB! Este documento fornece diretrizes para contribuições que ajudarão a manter a qualidade e consistência do projeto.

## 📋 Índice

- [Código de Conduta](#código-de-conduta)
- [Como Contribuir](#como-contribuir)
- [Configuração do Ambiente](#configuração-do-ambiente)
- [Processo de Desenvolvimento](#processo-de-desenvolvimento)
- [Padrões de Código](#padrões-de-código)
- [Testes](#testes)
- [Documentação](#documentação)
- [Pull Requests](#pull-requests)
- [Issues](#issues)
- [Revisão de Código](#revisão-de-código)

## 🤝 Código de Conduta

Este projeto adota o [Contributor Covenant](https://www.contributor-covenant.org/) como código de conduta. Ao participar, você concorda em manter um ambiente respeitoso e inclusivo para todos.

### Comportamento Esperado

- Use linguagem acolhedora e inclusiva
- Respeite diferentes pontos de vista e experiências
- Aceite críticas construtivas com graciosidade
- Foque no que é melhor para a comunidade
- Demonstre empatia com outros membros da comunidade

### Comportamento Inaceitável

- Linguagem ou imagens sexualizadas
- Trolling, comentários insultuosos ou depreciativos
- Assédio público ou privado
- Publicação de informações privadas sem permissão
- Outra conduta inadequada em um ambiente profissional

## 🚀 Como Contribuir

Existem várias maneiras de contribuir para o Guardian DB:

### 🐛 Reportar Bugs

- Use o template de issue para bugs
- Inclua passos para reproduzir o problema
- Forneça informações do ambiente (Rust version, OS, etc.)
- Adicione logs relevantes

### 💡 Sugerir Funcionalidades

- Use o template de issue para feature requests
- Descreva claramente o problema que resolve
- Explique por que seria útil para outros usuários
- Considere múltiplas soluções possíveis

### 📝 Melhorar Documentação

- Corrija erros de digitação ou gramática
- Adicione exemplos de código
- Melhore explicações existentes
- Traduza documentação

### 💻 Contribuir com Código

- Implemente novas funcionalidades
- Corrija bugs existentes
- Melhore performance
- Adicione testes

## ⚙️ Configuração do Ambiente

### Pré-requisitos

```bash
# Rust 1.70 ou superior
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Git
# Instale pelo gerenciador de pacotes do seu OS

# Editor recomendado: VS Code com rust-analyzer
```

### Configuração Inicial

```bash
# 1. Fork o repositório no GitHub

# 2. Clone seu fork
git clone https://github.com/wmaslonek/guardian-db.git
cd guardian-db

# 3. Adicione o repositório original como remote
git remote add upstream https://github.com/wmaslonek/guardian-db.git

# 4. Instale dependências e verifique se compila
cargo build

# 5. Execute os testes
cargo test

# 6. Verifique formatação
cargo fmt --check

# 7. Execute clippy
cargo clippy -- -D warnings
```

### Ferramentas Recomendadas

```bash
# Instalar ferramentas de desenvolvimento
rustup component add rustfmt clippy

# Para testes de benchmark
cargo install cargo-criterion

# Para análise de cobertura
cargo install cargo-tarpaulin

# Para documentação
cargo install mdbook
```

## 🔄 Processo de Desenvolvimento

### 1. Planejamento

1. Discuta grandes mudanças em issues primeiro
2. Verifique se não há work in progress similar
3. Entenda o impacto da mudança no projeto

### 2. Desenvolvimento

```bash
# 1. Crie uma branch para sua feature
git checkout -b feature/nova-funcionalidade

# 2. Faça commits pequenos e focados
git commit -m "tipo: descrição concisa"

# 3. Mantenha sua branch atualizada
git fetch upstream
git rebase upstream/main

# 4. Execute testes frequentemente
cargo test
```

### 3. Tipos de Commit

Use [Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` - Nova funcionalidade
- `fix:` - Correção de bug
- `docs:` - Mudanças na documentação
- `style:` - Formatação (sem mudança de código)
- `refactor:` - Refatoração de código
- `test:` - Adicionar ou modificar testes
- `chore:` - Mudanças em ferramentas, configs, etc.

Exemplos:
```bash
git commit -m "feat: adiciona suporte a document queries"
git commit -m "fix: corrige memory leak no replicator"
git commit -m "docs: atualiza exemplos do README"
```

## 📏 Padrões de Código

### Formatação

```bash
# Formate todo o código antes de commitar
cargo fmt

# Verifique se está formatado
cargo fmt --check
```

### Linting

```bash
# Execute clippy para verificar problemas
cargo clippy -- -D warnings

# Para código mais rigoroso
cargo clippy -- -D clippy::all
```

### Convenções Rust

#### Nomenclatura

```rust
// Structs: PascalCase
pub struct EventLogStore;

// Functions e variables: snake_case
pub fn create_database() -> Result<()>;
let database_name = "my-db";

// Constants: SCREAMING_SNAKE_CASE
const MAX_RETRIES: usize = 3;

// Traits: PascalCase, preferencialmente com sufixo descritivo
pub trait StorageProvider;
pub trait Replicatable;
```

#### Documentação

```rust
/// Cria uma nova instância de EventLogStore.
///
/// # Arguments
///
/// * `name` - Nome único para o store
/// * `options` - Opções de configuração
///
/// # Returns
///
/// Retorna `Result<EventLogStore, GuardianError>`
///
/// # Examples
///
/// ```rust
/// use guardian_db::EventLogStore;
///
/// let store = EventLogStore::new("my-log", None)?;
/// ```
///
/// # Errors
///
/// Retorna erro se:
/// - Nome já existe
/// - Configuração inválida
pub fn new(name: &str, options: Option<StoreOptions>) -> Result<Self> {
    // implementação
}
```

#### Error Handling

```rust
// Use thiserror para errors customizados
#[derive(Debug, thiserror::Error)]
pub enum GuardianError {
    #[error("Database not found: {name}")]
    DatabaseNotFound { name: String },
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("IPFS error: {0}")]
    Ipfs(String),
}

// Prefira Result<T> sobre unwrap/expect
pub fn get_database(name: &str) -> Result<Database> {
    databases.get(name)
        .ok_or_else(|| GuardianError::DatabaseNotFound { 
            name: name.to_string() 
        })
}
```

#### Async/Await

```rust
// Use async/await consistentemente
pub async fn replicate_data(&self) -> Result<()> {
    // operações assíncronas
}

// Para traits, use async-trait
#[async_trait]
pub trait Replicator {
    async fn start_replication(&self) -> Result<()>;
}
```

## 🧪 Testes

### Executando Testes

```bash
# Todos os testes
cargo test

# Testes específicos
cargo test test_event_log

# Testes com output
cargo test -- --nocapture

# Testes com logs
RUST_LOG=debug cargo test

# Testes de integração
cargo test --test integration

# Benchmarks
cargo bench
```

### Escrevendo Testes

#### Testes Unitários

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio_test;

    #[tokio::test]
    async fn test_create_database() {
        let db = GuardianDB::new_mock().await.unwrap();
        let result = db.log("test-db", None).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_error_handling() {
        let error = GuardianError::DatabaseNotFound { 
            name: "missing".to_string() 
        };
        assert_eq!(error.to_string(), "Database not found: missing");
    }
}
```

#### Testes de Integração

```rust
// tests/integration.rs
use guardian_db::*;

#[tokio::test]
async fn test_full_replication_flow() {
    let node1 = setup_node("node1").await;
    let node2 = setup_node("node2").await;
    
    // Criar database no node1
    let db1 = node1.log("shared-log", None).await.unwrap();
    db1.add(b"test data").await.unwrap();
    
    // Conectar nodes
    node1.connect_peer(&node2.peer_id()).await.unwrap();
    
    // Verificar replicação
    let db2 = node2.log("shared-log", None).await.unwrap();
    wait_for_replication(&db2, 1).await;
    
    assert_eq!(db2.iterator(None).await.unwrap().count(), 1);
}
```

### Cobertura de Testes

```bash
# Instalar tarpaulin
cargo install cargo-tarpaulin

# Executar com cobertura
cargo tarpaulin --out Html

# Ver relatório
open tarpaulin-report.html
```

## 📚 Documentação

### Documentação de Código

- Documente todas as funções públicas
- Use exemplos de código nos comentários
- Explique parâmetros complexos
- Documente comportamento de erro

### Documentação Externa

```bash
# Gerar documentação
cargo doc --open

# Verificar links quebrados
cargo doc --document-private-items

# Atualizar README
# Mantenha exemplos sincronizados com o código
```

### Exemplos

- Adicione exemplos na pasta `examples/`
- Mantenha exemplos simples e focados
- Teste exemplos como parte do CI

```rust
// examples/basic_usage.rs
use guardian_db::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Exemplo simples e funcional
    let db = GuardianDB::new_mock().await?;
    let log = db.log("example", None).await?;
    log.add(b"Hello, Guardian DB!").await?;
    Ok(())
}
```

## 🔀 Pull Requests

### Antes de Submeter

- [ ] Código compila sem warnings
- [ ] Todos os testes passam
- [ ] Código está formatado (`cargo fmt`)
- [ ] Clippy não reporta issues (`cargo clippy`)
- [ ] Documentação atualizada
- [ ] Testes adicionados para novas funcionalidades
- [ ] CHANGELOG.md atualizado (se aplicável)

### Template de PR

```markdown
## Descrição

Descrição clara do que este PR faz.

## Tipo de Mudança

- [ ] Bug fix (mudança que corrige um issue)
- [ ] Nova funcionalidade (mudança que adiciona funcionalidade)
- [ ] Breaking change (mudança que quebra compatibilidade)
- [ ] Documentação (mudança apenas na documentação)

## Como Testar

1. Compile o projeto
2. Execute `cargo test`
3. Teste específico: `cargo test test_nova_funcionalidade`

## Checklist

- [ ] Código segue padrões do projeto
- [ ] Testes passam localmente
- [ ] Documentação atualizada
- [ ] Sem warnings do clippy
```

### Processo de Review

1. **Automated Checks**: CI deve passar
2. **Code Review**: Pelo menos um maintainer aprova
3. **Testing**: Testes manuais se necessário
4. **Documentation**: Verificar se docs estão atualizadas
5. **Merge**: Squash commits se necessário

## 🐛 Issues

### Reportando Bugs

Use o template de bug report:

```markdown
**Descreva o bug**
Descrição clara do problema.

**Passos para Reproduzir**
1. Vá para '...'
2. Clique em '....'
3. Execute '....'
4. Veja o erro

**Comportamento Esperado**
O que deveria acontecer.

**Screenshots**
Se aplicável, adicione screenshots.

**Ambiente:**
 - OS: [e.g. Ubuntu 20.04]
 - Rust Version: [e.g. 1.70.0]
 - Guardian DB Version: [e.g. 0.1.0]

**Contexto Adicional**
Qualquer outra informação relevante.
```

### Sugerindo Funcionalidades

```markdown
**Funcionalidade Desejada**
Descrição clara da funcionalidade.

**Problema Resolvido**
Que problema esta funcionalidade resolve?

**Solução Proposta**
Como você gostaria que funcionasse?

**Alternativas Consideradas**
Outras soluções que você considerou?

**Contexto Adicional**
Screenshots, mockups, etc.
```

## 👥 Revisão de Código

### Para Reviewers

#### O que Verificar

- **Funcionalidade**: O código faz o que deveria?
- **Testes**: Mudanças estão testadas adequadamente?
- **Performance**: Há impactos de performance?
- **Security**: Há vulnerabilidades de segurança?
- **Design**: O design está bem pensado?
- **Documentação**: Está bem documentado?

#### Como Dar Feedback

```markdown
# ✅ Bom feedback
"Este método poderia beneficiar de error handling mais específico. 
Considere usar `GuardianError::InvalidInput` em vez de genérico."

# ❌ Feedback ruim  
"Este código está errado."
```

#### Checklist de Review

- [ ] Código compila e teste passam
- [ ] Lógica está correta
- [ ] Error handling adequado
- [ ] Documentação clara
- [ ] Performance aceitável
- [ ] Sem vazamentos de memória
- [ ] Thread safety (se aplicável)

### Para Autores

#### Respondendo a Feedback

- Seja receptivo a sugestões
- Faça perguntas se não entender
- Explique decisões de design quando relevante
- Atualize o código baseado no feedback

#### Após Aprovação

```bash
# Rebasing se necessário
git rebase -i upstream/main

# Squash commits relacionados
# Force push se necessário (em sua branch)
git push --force-with-lease origin feature/minha-feature
```

## 📊 Métricas e Qualidade

### Objetivos de Qualidade

- **Cobertura de testes**: > 85%
- **Performance**: Sem regressões significativas
- **Documentação**: Todas as APIs públicas documentadas
- **Clippy warnings**: Zero warnings
- **Memory leaks**: Zero vazamentos

### Ferramentas de Monitoramento

```bash
# Benchmark contínuo
cargo bench

# Profile de memória
valgrind --tool=memcheck target/debug/guardian-db

# Análise de dependências
cargo audit

# Verificação de licenças
cargo license
```

## 🎯 Áreas Prioritárias

### Para Novos Contribuidores

1. **Documentação**: Melhorar exemplos e tutoriais
2. **Testes**: Adicionar testes de integração
3. **Error handling**: Melhorar mensagens de erro
4. **Performance**: Benchmarks e otimizações

### Para Contribuidores Experientes

1. **Core features**: Novas funcionalidades do store
2. **IPFS integration**: Melhorar integração nativa
3. **P2P networking**: Otimizar comunicação entre peers
4. **Security**: Auditoria e melhorias de segurança

## 📞 Comunicação

### Canais

- **GitHub Issues**: Bugs, features, discussões técnicas
- **GitHub Discussions**: Perguntas gerais, ideias
- **Discord**: Chat em tempo real (link no README)

### Respondendo Issues

- Responda dentro de 48 horas quando possível
- Seja helpful e construtivo
- Encaminhe para experts quando necessário
- Use labels apropriados

## 🏆 Reconhecimento

Contribuidores são reconhecidos através de:

- **All Contributors**: Bot que adiciona contribuidores ao README
- **Release Notes**: Menção em changelogs
- **Hall of Fame**: Seção especial para contribuidores frequentes

## 📝 Conclusão

Obrigado por contribuir para o Guardian DB! Sua ajuda é essencial para tornar este projeto melhor para toda a comunidade.

### Recursos Úteis

- [Rust Book](https://doc.rust-lang.org/book/)
- [Async Programming in Rust](https://rust-lang.github.io/async-book/)
- [API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [OrbitDB Docs](https://orbitdb.org/) (referência)

### Dúvidas?

Se tiver alguma dúvida sobre contribuições, abra uma issue com a label `question` ou entre em contato através dos canais de comunicação.

---

**Happy coding!** 🦀
