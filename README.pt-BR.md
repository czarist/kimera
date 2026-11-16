# Kimera

**Uma ferramenta de linha de comando para os apps Craft.**

[English](README.md) · Português (Brasil) · [Roadmap](ROADMAP.md) · [Changelog](CHANGELOG.md)

Funciona com LightCraft, PhotoCraft e VectorCraft, da equipe ArtCraft. Não é afiliado à equipe
ArtCraft nem endossado por ela.

O Kimera encadeia os apps Craft pelas ferramentas de linha de comando de cada um, um programa por
vez, passando arquivos de uma etapa para a outra. Ele nunca altera os apps. Cada execução grava
numa pasta própria, e os originais nunca são sobrescritos.

```
sua pasta (só leitura)
  └─► work/run-<hora>/
        1-photocraft/   limpeza, trabalho de pixel  (photocraft-cli run)
        2-lightcraft/   máscaras, cor               (lightcraft-cli run --import … app.export)
```

As etapas rodam na ordem da receita, uma foto por vez; a saída de uma etapa é a entrada da
próxima. Os dois apps rodam sem janela, então nenhum precisa estar aberto. Uma foto que falha
para ali e as outras continuam.

## Instalação

1. Compile: `cargo build --release` (o binário fica em `target/release/kimera.exe`).
2. Copie `kimera.toml.example` para `kimera.toml` e ajuste os caminhos do `lightcraft-cli`, do
   `photocraft-cli` e da pasta de trabalho.
3. Confira tudo: `kimera doctor`.

## Comandos

| Comando | O que faz |
|---|---|
| `kimera doctor` | Encontra os apps, mostra as versões e diz se a porta de controle do LightCraft responde |
| `kimera run --recipe recipes/cleanup-cinematic.toml D:\ensaio` | Roda as etapas da receita em todas as imagens de uma pasta |
| `kimera run --recipe … a.jpg b.jpg` | O mesmo, só em alguns arquivos |
| `kimera pipeline --recipe … [--ids 12,15]` | Exporta fotos de um LightCraft aberto (`lightcraft --control 7980`; a seleção ou os ids) e roda as etapas nelas |

## Receitas

Uma receita é um arquivo TOML: uma lista de `[[stage]]`, cada uma com um app e os comandos dele,
em ordem. Para listar os comandos: `photocraft-cli commands` ou `lightcraft-cli commands`.

```toml
[[stage]]
app = "photocraft"
out_ext = "tif"        # o que a próxima etapa lê; "pcraft" mantém camadas (só na última)
steps = [{ cmd = "select.subject" }]

[[stage]]
app = "lightcraft"
steps = [
  { cmd = "mask.add", params = { kind = "background" } },
  { cmd = "mask.adjust", params = { values = { exposure = -0.35 } } },
]
export = { format = "jpg", quality = 92 }   # também long_edge, bit_depth, color_space, options
```

No `kimera pipeline`, uma tabela `[export]` (`format`, `bit_depth`, `long_edge`, `color_space`)
define como as fotos saem do catálogo do LightCraft. Ids de comando e extensões são conferidos
antes de qualquer coisa rodar. O [`prompts/playbook.md`](prompts/playbook.md) liga os pedidos do
dia a dia aos comandos.

## Regras de projeto

- **Nunca travar:** nada de `unwrap`, `expect` ou `panic!` fora dos testes, e nenhum `unsafe`.
  Toda falha vira uma mensagem de erro.
- **Só conexões locais:** o Kimera fala com o canal de controle do LightCraft em `127.0.0.1` e
  recusa outros endereços.
- **Não destrutivo:** a saída do PhotoCraft nunca substitui a entrada, e cada execução ganha uma
  pasta `run-<hora>` nova.

## Licença

MIT OU Apache-2.0.
