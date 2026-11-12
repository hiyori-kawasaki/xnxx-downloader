# Changelog

Formato basado en [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/).

## [Unreleased] - 2026-11-10

### Corregido

- **`profile`: devolvía "0 videos encontrados" en perfiles `/models/...`.**
  - Causa raíz: el endpoint `<perfil>/videos/new/<n>` sí respondía 200, pero con JSON
    (`{"type":"uploads","nb_videos":31,"nb_per_page":36,"current_page":0,"videos":[...]}`)
    cuyos enlaces tienen la forma `/prof-video-click/pornstar/<perfil>/<eid>/<slug>`.
    El regex `VIDEO_PATH` solo reconocía `/video.<id>/<slug>`, `/video-<id>/<slug>` y
    `/video<digitos>/<slug>`, así que no encontraba ningún video.
  - Segundo problema: una página 0 vacía hacía `break` y la función devolvía
    `Ok(vec![])`, indistinguible de "el perfil no tiene videos". El fallo era silencioso.
  - Resultado: con el perfil de prueba ahora se listan los videos gratuitos del perfil
    (31 según `nb_videos`) en lugar de 0.

### Añadido

- **Reconocimiento de enlaces `/prof-video-click/<tipo>/<perfil>/<eid>/<slug>`** en
  `extract_video_links`. Se normalizan al enlace real de la página del video:
  `https://<host>/video.<eid>/<slug>`, el mismo formato que ya consume `download_best`.
  El `id` de deduplicación pasa a ser `video.<eid>` para esos enlaces, igual que en los
  enlaces directos.
- **Prueba de varias secciones AJAX** (`SECTIONS`: `videos/new`, `videos/best`, `videos`).
  En cada una se pide la página 0 y se usa la primera que devuelva videos. Si una
  sección responde error (p. ej. 404) o viene vacía, se pasa a la siguiente.
  - Nota: son suposiciones razonables, no endpoints documentados. En la prueba real
    `videos/new` y `videos/best` devolvieron JSON con el mismo formato y `videos` dio 404.
- **Fallback al HTML del propio perfil** cuando ninguna sección AJAX produce videos.
  Devuelve una lista parcial (en el perfil de prueba, 8 videos de la muestra incrustada
  en la pestaña "About me") y emite un `warn` indicando que es parcial.
- **Error explícito `Error::NoProfileVideos(String)`** (en `error.rs`) cuando no hay
  videos ni en las secciones AJAX ni en el HTML. El mensaje sugiere usar
  `RUST_LOG=debug`.
- **Trazas de depuración** (`RUST_LOG=debug`):
  - cuántos videos nuevos aporta cada página;
  - qué sección funcionó y cuántos videos dio;
  - si una sección falla o viene vacía, el motivo;
  - si la página 0 viene vacía, los primeros 300 caracteres de la respuesta
    (`preview`). Esto fue lo que permitió identificar el formato JSON real.
- **Tests nuevos** en `profile.rs`:
  - `extracts_links_from_profile_json_prof_video_click`: JSON real con enlaces
    `/prof-video-click/` escapados (`\/`), comprueba URL final, orden y título.
  - `extracts_links_from_profile_html_with_long_slugs`: enlaces `/video.<id>/<slug>`
    con slugs largos y guiones, como los del HTML del perfil.

### Cambiado

- **`profile.rs`: la lógica de listado se divide en dos funciones.**
  - `collect_section`: recorre `<perfil>/<sección>/<n>` hasta que una página no aporte
    videos nuevos o se alcance `max_pages`. Un error en la página 0 se propaga (la
    sección no sirve); un error en páginas posteriores solo corta el recorrido con `warn`.
  - `list_profile_videos`: orquesta secciones, fallback y error final.
- **`extract_video_links`** pasa de `find_iter` a `captures_iter` para distinguir los
  dos formatos de enlace mediante grupos de captura. También quita puntos finales del
  slug (antes solo se hacía sobre el path completo) y construye la URL como
  `/{id}/{slug}` sobre el host base.
- Documentación del módulo actualizada: describe las secciones, el fallback y el error.

### Sin cambios

- `main.rs`, `lib.rs`, `download.rs`, `hls.rs`, `http.rs`, `player.rs`, `filename.rs`.
  La API pública (`list_profile_videos`, `extract_video_links`, `VideoLink`) mantiene
  su firma.

### Notas y limitaciones conocidas

- **Videos RED/premium:** el perfil de prueba muestra 35 videos RED además de los 31
  gratuitos. No se listan ni se pueden descargar con esta herramienta.
- **Pestaña "Latest":** es un feed con carga dinámica distinto de la pestaña "Videos".
  El scroll que se detiene en el navegador pertenece a ese feed y no afecta a este
  listado. No hay soporte para ese feed.
- **Paginación:** el JSON indica `nb_per_page: 36`. Con 31 videos todo cabe en la
  página 0. En perfiles con más videos el recorrido sigue pidiendo `<n>` = 1, 2, ...
  hasta que una página no aporte nada nuevo; esto no se ha probado contra un perfil
  de varias páginas.
- **Ayuda de `--max-pages`:** en `main.rs` sigue diciendo "pestaña 'Latest'", aunque el
  código recorre la pestaña "Videos". Pendiente de ajustar el texto.
- **Fallback parcial:** si se llega al HTML del perfil, la lista puede incluir videos
  de cuentas relacionadas ("worked for/with"), no solo del perfil consultado.
- **Verificación:** la extracción se probó con tests en un crate aislado y contra el
  perfil de prueba ejecutando el binario compilado. No hay pruebas automáticas contra
  el sitio real.

### Pendiente / ideas

- Corregir el texto de ayuda de `--max-pages` en `main.rs`.
- Ajustar `SECTIONS` por tipo de perfil (`models`, `profiles`, `pornstars`) si algún
  tipo requiere otra sección.
- Leer `nb_videos` / `nb_per_page` del JSON para decidir cuántas páginas pedir en vez de
  depender de que una página salga vacía.
