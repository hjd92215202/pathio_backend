-- 1. 用户表 (Users)
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) UNIQUE NOT NULL,
    password_hash VARCHAR(255) NOT NULL,
    nickname VARCHAR(50),
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 2. 组织表 (Organizations)
CREATE TABLE organizations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(100) NOT NULL,
    owner_id UUID REFERENCES users(id), -- 组织创建者
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 3. 组织成员关联表 (Org_Members)
CREATE TABLE org_members (
    org_id UUID REFERENCES organizations(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(20) DEFAULT 'member', -- admin, editor, member(只读)
    PRIMARY KEY (org_id, user_id)
);

-- 4. 路线图空间 (Roadmaps)
CREATE TABLE roadmaps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID REFERENCES organizations(id) ON DELETE CASCADE,
    title VARCHAR(255) NOT NULL,
    description TEXT,
    share_token VARCHAR(100) UNIQUE, -- 用于只读分享的唯一链接参数
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 5. 画布节点 (Nodes)
CREATE TABLE nodes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    roadmap_id UUID REFERENCES roadmaps(id) ON DELETE CASCADE,
    title VARCHAR(255) NOT NULL,
    status VARCHAR(20) DEFAULT 'todo', -- todo, doing, done
    assignee_id UUID REFERENCES users(id) ON DELETE SET NULL, -- 任务分配给谁
    pos_x FLOAT NOT NULL, -- 画布上的 X 坐标
    pos_y FLOAT NOT NULL, -- 画布上的 Y 坐标
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 6. 画布连线 (Edges)
CREATE TABLE edges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    roadmap_id UUID REFERENCES roadmaps(id) ON DELETE CASCADE,
    source_node_id UUID REFERENCES nodes(id) ON DELETE CASCADE,
    target_node_id UUID REFERENCES nodes(id) ON DELETE CASCADE,
    UNIQUE(source_node_id, target_node_id) -- 防止重复连线
);

-- 7. 笔记表 (Notes)
CREATE TABLE notes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    node_id UUID UNIQUE REFERENCES nodes(id) ON DELETE CASCADE, -- 一对一：一个节点对应一篇主笔记
    content JSONB, -- 推荐存 JSONB，方便适配类似 Notion 的 Block 富文本数据
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE nodes ALTER COLUMN roadmap_id DROP NOT NULL;


-- 1. 创建一个名为“我的深度研究路径”的路线图，并设置分享 token
INSERT INTO roadmaps (id, title, share_token) 
VALUES (gen_random_uuid(), '我的第一个研究路径', 'path123');

-- 2. 将你目前所有的节点都归属到这个路线图下
UPDATE nodes SET roadmap_id = (SELECT id FROM roadmaps WHERE share_token = 'path123');

-- 3. 将你目前所有的连线也归属过去
UPDATE edges SET roadmap_id = (SELECT id FROM roadmaps WHERE share_token = 'path123');

-- 1. 确保 nickname 不能为空 (请确保表中现有数据 nickname 都有值，否则会报错)
ALTER TABLE users ALTER COLUMN nickname SET NOT NULL;

-- 2. 确保 nickname 全局唯一（因为要用来登录）
ALTER TABLE users ADD CONSTRAINT users_nickname_unique UNIQUE (nickname);