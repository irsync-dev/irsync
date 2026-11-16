-- Pandoc filter for tools/build-man.sh: turns a page header and the book
-- chapters after it into the sections of one man page.
--
-- The script puts "<!-- chapter: book/src/PATH -->" before each chapter and
-- "<!-- end of chapters -->" before the rest of the header. Blocks before the
-- first marker and after the last one are the header's, and stay as written.

-- The man page each chapter is part of, by its path under book/src, and the
-- chapter's title.
local PAGE = {
  ["reference/command-line.md"] = { "irsync(1)", "Command line" },
  ["reference/management.md"] = { "irsync(1)", "Management commands" },
  ["reference/configuration.md"] = { "irsync.toml(5)", "Configuration" },
  ["fileset/reference.md"] = { "fileset(1)", "fileset reference" },
}
local BOOK = "https://irsync.org/book/"

local function upper(inlines)
  return pandoc.text.upper(pandoc.utils.stringify(inlines))
end

-- Resolves a relative link path against a chapter's directory.
local function resolve(dir, path)
  local parts = {}
  for part in (dir .. path):gmatch("[^/]+") do
    if part == ".." then
      table.remove(parts)
    elseif part ~= "." then
      table.insert(parts, part)
    end
  end
  return table.concat(parts, "/")
end

-- Rewrites one link found in `chapter` (nil in the header) of `page`.
-- Inside a table, `notes` is a list: a link into the book then keeps only
-- its text, and its text and URL are added to `notes` for a paragraph after
-- the table. A URL is wider than most columns, and mandoc widens a column to
-- its longest word, which pushes the table past the right edge.
local function link(el, chapter, page, notes)
  local target = el.target
  if target:match("^%a[%w+.-]*:") then
    -- An absolute URL stays. An autolink is written as plain text: as a
    -- link with no text of its own, mandoc warns about an empty block.
    if pandoc.utils.stringify(el.content) == target then
      return pandoc.Str(target)
    end
    return el
  end
  if target:match("^#") then
    return el.content
  end
  local path, anchor = target:match("^([^#]*)(.*)$")
  if not chapter or not path:match("%.md$") then
    return el
  end
  path = resolve(chapter:match("^(.*/)") or "", path)
  local text = el.content
  local other = PAGE[path] and PAGE[path][1]
  if other == page then
    return text
  end
  -- "See [Configuration](configuration.md)" becomes "See irsync.toml(5)";
  -- other text is followed by the page or the book's URL.
  if other and pandoc.utils.stringify(text) == PAGE[path][2] then
    return pandoc.Inlines({ pandoc.Str(other) })
  end
  local where = other or BOOK .. path:gsub("%.md$", ".html") .. anchor
  if notes and not other then
    for _, note in ipairs(notes) do
      if note.url == where then
        return text
      end
    end
    table.insert(notes, { text = text:clone(), url = where })
    return text
  end
  text:insert(pandoc.Space())
  text:insert(pandoc.Str("(" .. where .. ")"))
  return text
end

-- The paragraph after a table that gives the URLs of its links.
local function link_notes(notes)
  local para = pandoc.Inlines({})
  for i, note in ipairs(notes) do
    if i > 1 then
      para:insert(pandoc.LineBreak())
    end
    para:extend(note.text)
    para:insert(pandoc.Str(":"))
    para:insert(pandoc.Space())
    para:insert(pandoc.Str(note.url))
  end
  return pandoc.Para(para)
end

-- Text width of a man page's body at 80 columns, in characters, which is
-- what pandoc makes of a table column width of 1.0. A table takes 3 more
-- between columns and 1 at the end.
local WIDTH = 70

-- The inlines of a table cell, or nil when it holds more than one paragraph.
local function cell_inlines(cell)
  if #cell.contents == 0 then
    return pandoc.Inlines({})
  end
  if #cell.contents == 1 and (cell.contents[1].t == "Plain" or cell.contents[1].t == "Para") then
    return cell.contents[1].content
  end
end

-- A table as a list of its rows: the first cell is the term, then one line
-- "Header: value" per middle cell, then the last cell. This is the usual
-- man page layout for a list of options.
local function as_list(t)
  local labels = {}
  for i, cell in ipairs(t.head.rows[1].cells) do
    labels[i] = pandoc.utils.stringify(cell.contents)
  end
  local items = {}
  for _, body in ipairs(t.bodies) do
    for _, row in ipairs(body.body) do
      local term = cell_inlines(row.cells[1])
      if not term then
        return nil
      end
      local defn = pandoc.Blocks({})
      for i = 2, #row.cells - 1 do
        local value = cell_inlines(row.cells[i])
        if not value then
          return nil
        end
        local line = pandoc.Inlines({ pandoc.Str(labels[i] .. ":"), pandoc.Space() })
        line:extend(value)
        defn:insert(pandoc.Plain(line))
      end
      defn:extend(row.cells[#row.cells].contents)
      table.insert(items, { term, { defn } })
    end
  end
  return pandoc.DefinitionList(items)
end

-- Gives every table column a width, so that a table fits in 80 columns.
-- Without widths, a column of long text runs off the right edge. Each
-- column gets what its longest cell needs, or an equal share of what is
-- left when that is not enough, and never less than its longest word.
-- mandoc does not break a word, so a table whose longest words alone are
-- too wide becomes a list instead.
function Table(t)
  local n = #t.colspecs
  local need, word = {}, {}
  for i = 1, n do
    need[i], word[i] = 1, 1
  end
  local function measure(rows)
    for _, row in ipairs(rows) do
      for i, cell in ipairs(row.cells) do
        local text = pandoc.utils.stringify(cell.contents)
        need[i] = math.max(need[i], utf8.len(text))
        for w in text:gmatch("%S+") do
          word[i] = math.max(word[i], utf8.len(w))
        end
      end
    end
  end
  measure(t.head.rows)
  for _, body in ipairs(t.bodies) do
    measure(body.body)
  end
  local left, open = WIDTH - 1 - 3 * (n - 2), n
  local words = 0
  for i = 1, n do
    words = words + word[i]
  end
  if words > left and #t.head.rows == 1 then
    local list = as_list(t)
    if list then
      return list
    end
  end
  local width = {}
  -- Settle the narrow columns first, so the wide ones share the rest.
  local order = {}
  for i = 1, n do
    order[i] = i
  end
  table.sort(order, function(a, b) return need[a] < need[b] or (need[a] == need[b] and a < b) end)
  for _, i in ipairs(order) do
    width[i] = math.max(word[i], math.min(need[i], left // open))
    left, open = left - width[i], open - 1
  end
  for i = 1, n do
    t.colspecs[i][2] = width[i] / WIDTH
  end
  return t
end

local function section(title)
  return pandoc.Header(1, title)
end

-- Moves the named level 1 sections, in their order, to just before SEE ALSO.
local function move_before_see_also(blocks, names)
  local sections, moved = {}, {}
  local current = {}
  for _, b in ipairs(blocks) do
    if b.t == "Header" and b.level == 1 then
      current = {}
      table.insert(sections, { title = pandoc.utils.stringify(b.content), blocks = current })
    end
    table.insert(current, b)
  end
  local result = pandoc.Blocks({})
  for _, s in ipairs(sections) do
    if names[s.title] then
      table.insert(moved, s)
    end
  end
  for _, s in ipairs(sections) do
    if s.title == "SEE ALSO" then
      for _, m in ipairs(moved) do
        result:extend(m.blocks)
      end
    end
    if not names[s.title] then
      result:extend(s.blocks)
    end
  end
  return result
end

function Pandoc(doc)
  local out = pandoc.Blocks({})
  local chapter, first, page
  local intro = false -- in the first chapter, before its first ##
  local intro_section -- the section the intro text went under
  for _, b in ipairs(doc.blocks) do
    local marker = b.t == "RawBlock" and b.text:match("^<!%-%- chapter: book/src/(%S+) %-%->%s*$")
    if marker then
      chapter = marker
      first = page == nil
      page = page or (PAGE[chapter] or {})[1]
      if not PAGE[chapter] then
        error("filter.lua: no man page for chapter " .. chapter)
      end
      intro = first
    elseif b.t == "RawBlock" and b.text:match("^<!%-%- end of chapters %-%->%s*$") then
      chapter = nil
    else
      local c, notes = chapter, b.t == "Table" and {} or nil
      b = pandoc.walk_block(b, { Link = function(el) return link(el, c, page, notes) end })
      if chapter and b.t == "Header" then
        local level = first and b.level - 1 or b.level
        if level >= 1 then
          intro = false
          b.level = level
          if pandoc.utils.stringify(b.content) == "Exit codes" then
            b.content = { pandoc.Str("EXIT STATUS") }
          end
          out:insert(b)
        end
      elseif intro then
        if not intro_section and b.t == "CodeBlock" then
          intro_section = "SYNOPSIS"
          out:insert(section("SYNOPSIS"))
        elseif intro_section ~= "DESCRIPTION" and not (intro_section and b.t == "CodeBlock") then
          intro_section = "DESCRIPTION"
          out:insert(section("DESCRIPTION"))
        end
        out:insert(b)
      else
        out:insert(b)
        if notes and #notes > 0 then
          out:insert(link_notes(notes))
        end
      end
    end
  end
  -- Man section titles are upper case.
  for _, b in ipairs(out) do
    if b.t == "Header" and b.level == 1 then
      b.content = { pandoc.Str(upper(b.content)) }
    end
  end
  if page == "irsync(1)" then
    out = move_before_see_also(out, { ["ENVIRONMENT"] = true, ["EXIT STATUS"] = true })
  end
  doc.blocks = out
  return doc
end
